// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::app::{Block, Status};
use lattice_api::local::{ModelAttemptKey, NodeId, TurnId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BlockId {
    Block(u128),
    Comment {
        owner: u128,
        number: usize,
        occurrence: usize,
    },
}

impl BlockId {
    pub(crate) fn comment(self, number: usize, occurrence: usize) -> Self {
        let owner = match self {
            Self::Block(owner) | Self::Comment { owner, .. } => owner,
        };
        Self::Comment {
            owner,
            number,
            occurrence,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct IdentifiedBlock {
    pub(crate) id: BlockId,
    pub(crate) block: Block,
}

/// 一段内块的位置槽位。替换一段时同一槽位继承原来的身份，浏览光标与 Dock 因此留在原块上：
/// 调用按 InvocationId，重试提示按尝试，结局按 TurnId，其余按同类块在段内的序号。
#[derive(PartialEq, Eq, Hash)]
enum Slot {
    Retry(ModelAttemptKey),
    Closing(TurnId),
    Tool(String),
    Kind(u8, usize),
}

fn slot_of(block: &Block, counts: &mut [usize; 6]) -> Slot {
    let kind = match block {
        Block::Status(Status {
            retry: Some(key), ..
        }) => return Slot::Retry(*key),
        Block::Status(Status {
            closing: Some((turn, _)),
            ..
        }) => return Slot::Closing(*turn),
        Block::Tool { id, .. } => return Slot::Tool(id.clone()),
        Block::UserTurn { .. } | Block::Step { .. } => 0,
        Block::User(_) | Block::CommentedUser(_) | Block::RejectedUser(_) => 1,
        // 生成中的文字由条目中的回复取代时仍是同一块。
        Block::Assistant(_) | Block::HistoryAssistant(_) => 2,
        Block::AssistantStart => 3,
        Block::Status(_) => 4,
    };
    let n = counts[kind];
    counts[kind] = n.saturating_add(1);
    Slot::Kind(kind as u8, n)
}

/// 增删/重排携带身份；正文与工具状态可以原地更新。
#[derive(Default, Debug)]
pub(crate) struct BlockList {
    pub(crate) entries: Vec<IdentifiedBlock>,
    next_id: u128,
}

impl BlockList {
    pub(crate) fn allocate_id(&mut self) -> BlockId {
        let id = BlockId::Block(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    pub(crate) fn identify(&mut self, block: Block) -> IdentifiedBlock {
        let id = self.allocate_id();
        IdentifiedBlock { id, block }
    }

    pub(crate) fn push(&mut self, block: Block) {
        let entry = self.identify(block);
        self.entries.push(entry);
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &Block> + ExactSizeIterator {
        self.entries.iter().map(|entry| &entry.block)
    }

    pub(crate) fn iter_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = &mut Block> + ExactSizeIterator {
        self.entries.iter_mut().map(|entry| &mut entry.block)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 从 `start` 的段开头起，到下一个段开头或不属于任何节点的状态消息为止。
    /// 状态消息出现后不移动，所以条目原位替换时它留在条目之后。
    pub(crate) fn group(&self, start: usize) -> std::ops::Range<usize> {
        let end = self
            .entries
            .iter()
            .enumerate()
            .skip(start.saturating_add(1))
            .find(|(_, entry)| match &entry.block {
                Block::UserTurn { .. } | Block::Step { .. } => true,
                Block::Status(status) => status.retry.is_none() && status.closing.is_none(),
                _ => false,
            })
            .map_or(self.entries.len(), |(index, _)| index);
        start..end
    }

    /// 节点的段开头位置。
    pub(crate) fn node_start(&self, node: NodeId) -> Option<usize> {
        self.entries.iter().position(|entry| {
            matches!(entry.block,
                Block::UserTurn { node: Some(known), .. } | Block::Step { node: known, .. }
                    if known == node)
        })
    }

    /// 用 `blocks` 替换 `range`，按槽位继承身份；返回替换后的范围。
    pub(crate) fn replace(
        &mut self,
        range: std::ops::Range<usize>,
        blocks: Vec<Block>,
    ) -> std::ops::Range<usize> {
        let mut counts = [0; 6];
        let mut available = std::collections::HashMap::new();
        for entry in &self.entries[range.clone()] {
            available.insert(slot_of(&entry.block, &mut counts), entry.id);
        }
        let mut counts = [0; 6];
        let replacement: Vec<_> = blocks
            .into_iter()
            .map(|block| {
                let id = available
                    .remove(&slot_of(&block, &mut counts))
                    .unwrap_or_else(|| self.allocate_id());
                IdentifiedBlock { id, block }
            })
            .collect();
        let start = range.start;
        let len = replacement.len();
        self.entries.splice(range, replacement);
        start..start.saturating_add(len)
    }

    #[cfg(test)]
    pub(crate) fn extend(&mut self, blocks: impl IntoIterator<Item = Block>) {
        for block in blocks {
            self.push(block);
        }
    }
}

#[cfg(test)]
impl std::ops::Index<usize> for BlockList {
    type Output = Block;
    fn index(&self, index: usize) -> &Block {
        &self.entries[index].block
    }
}

#[cfg(test)]
impl From<Vec<Block>> for BlockList {
    fn from(blocks: Vec<Block>) -> Self {
        let mut list = Self::default();
        list.extend(blocks);
        list
    }
}
