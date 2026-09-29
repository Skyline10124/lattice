// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::app::Block;
use lattice_api::local::TurnId;

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

/// 一个节点内的显示块位置，不把无正文的作用域、接受标记和独立状态算入位置。
type IdentitySlot = (
    BlockId,
    ((TurnId, Option<u32>), usize, std::mem::Discriminant<Block>),
);

pub(crate) fn identity_slots(entries: &[IdentifiedBlock]) -> Vec<IdentitySlot> {
    let mut node = None;
    let mut position = 0usize;
    entries
        .iter()
        .filter_map(|entry| {
            match &entry.block {
                Block::UserTurn(id, _) => {
                    node = Some((*id, None));
                    position = 0;
                }
                Block::Step(key) => {
                    node = Some((key.turn_id, Some(key.ordinal)));
                    position = 0;
                }
                Block::AgentName(_) | Block::AssistantStart { .. } | Block::Status { .. } => {
                    return None;
                }
                _ => {}
            }
            let slot = node.map(|node| {
                (
                    entry.id,
                    (
                        node,
                        position,
                        match entry.block {
                            Block::HistoryAssistant(_) => {
                                std::mem::discriminant(&Block::Assistant(String::new()))
                            }
                            _ => std::mem::discriminant(&entry.block),
                        },
                    ),
                )
            });
            position = position.saturating_add(1);
            slot
        })
        .collect()
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

    pub(crate) fn last_mut(&mut self) -> Option<&mut Block> {
        self.entries.last_mut().map(|entry| &mut entry.block)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn inherit_discarded(&mut self, old: Vec<IdentitySlot>, new: Vec<IdentitySlot>) {
        let retained: std::collections::HashSet<_> = self.entries.iter().map(|e| e.id).collect();
        let mut available = std::collections::HashMap::<_, std::collections::VecDeque<_>>::new();
        for (id, slot) in old {
            if !retained.contains(&id) {
                available.entry(slot).or_default().push_back(id);
            }
        }
        let new: std::collections::HashMap<_, _> = new.into_iter().collect();
        for entry in &mut self.entries {
            if let Some(slot) = new.get(&entry.id)
                && let Some(id) = available.get_mut(slot).and_then(|ids| ids.pop_front())
            {
                entry.id = id;
            }
        }
        debug_assert!(
            {
                let mut ids = std::collections::HashSet::new();
                self.entries.iter().all(|entry| ids.insert(entry.id))
            },
            "Block identifiers must be unique"
        );
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
impl std::ops::IndexMut<usize> for BlockList {
    fn index_mut(&mut self, index: usize) -> &mut Block {
        &mut self.entries[index].block
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
