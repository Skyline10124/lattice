// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::input::KeyAction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Dock,
    Transcript(BrowseCursor),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BrowseCursor {
    pub(crate) block: BlockPosition,
    pub(crate) row: usize,
    pub(crate) desired_col: usize,
}

impl Transcript {
    /// 返回原 Block 是否仍有可显示行；消失时必须落在替代块开头。
    pub(crate) fn relocate(&self, cursor: &mut BrowseCursor) -> bool {
        if let Some((row, _)) = self
            .owners
            .iter()
            .enumerate()
            .filter(|(_, owner)| owner.id == cursor.block.id)
            .min_by_key(|(_, owner)| owner.row.abs_diff(cursor.block.row))
        {
            cursor.row = row;
            return true;
        } else if let Some((row, _)) = self
            .owners
            .iter()
            .enumerate()
            .filter(|(_, owner)| owner.row == 0)
            .min_by_key(|(row, _)| (row.abs_diff(cursor.row), *row))
        {
            cursor.row = row;
            cursor.desired_col = 0;
        }
        false
    }

    pub(crate) fn cursor(&self, row: usize, desired_col: usize) -> Option<BrowseCursor> {
        self.owners.get(row).copied().map(|block| BrowseCursor {
            block,
            row,
            desired_col,
        })
    }
}

impl BrowseCursor {
    pub(crate) fn reanchor(&mut self, transcript: &Transcript) {
        if let Some(block) = transcript.owners.get(self.row) {
            self.block = *block;
        }
    }

    pub(crate) fn move_in(&mut self, action: &KeyAction, lines: &[Line<'_>], width: u16) {
        match action {
            KeyAction::Up => self.row = self.row.saturating_sub(1),
            KeyAction::Down => {
                self.row = self
                    .row
                    .saturating_add(1)
                    .min(lines.len().saturating_sub(1))
            }
            KeyAction::Home => self.desired_col = 0,
            KeyAction::End => {
                self.desired_col = *columns(lines.get(self.row), width).last().unwrap_or(&0)
            }
            KeyAction::Left | KeyAction::Right => {
                let current = self.column(lines, width);
                let columns = columns(lines.get(self.row), width);
                self.desired_col = if matches!(action, KeyAction::Left) {
                    columns
                        .into_iter()
                        .rev()
                        .find(|&col| col < current)
                        .unwrap_or(current)
                } else {
                    columns
                        .into_iter()
                        .find(|&col| col > current)
                        .unwrap_or(current)
                };
            }
            _ => {}
        }
    }

    pub(crate) fn column(&self, lines: &[Line<'_>], width: u16) -> usize {
        columns(lines.get(self.row), width)
            .into_iter()
            .rev()
            .find(|&col| col <= self.desired_col)
            .unwrap_or(0)
    }
}

// 跳过 gutter；零宽字符不产生停靠格，宽字符只停在起点。
fn columns(line: Option<&Line<'_>>, width: u16) -> Vec<usize> {
    let mut columns = vec![0];
    let mut cell = 0usize;
    if let Some(line) = line {
        for glyph in line.styled_graphemes(Style::default()) {
            let cells = glyph.symbol.width();
            if cells > 0 && cell >= usize::from(GUTTER) {
                let col = cell.saturating_sub(usize::from(GUTTER));
                if col.saturating_add(cells) <= usize::from(width) && col > 0 {
                    columns.push(col);
                }
            }
            cell = cell.saturating_add(cells);
        }
    }
    columns
}

impl Viewport {
    pub(crate) fn keep_screen_row(&mut self, row: usize, screen_row: usize) {
        self.scroll = row
            .saturating_sub(screen_row)
            .min(self.rows.saturating_sub(self.height));
    }

    pub(crate) fn pause(&mut self) {
        self.follow = false;
    }

    pub(crate) fn clamp_cursor(&self, cursor: &mut BrowseCursor) {
        cursor.row = cursor.row.max(self.scroll).min(
            self.scroll
                .saturating_add(self.height.saturating_sub(1))
                .min(self.rows.saturating_sub(1)),
        );
    }

    pub(crate) fn reveal(&mut self, row: usize) {
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll.saturating_add(self.height) {
            self.scroll = row.saturating_sub(self.height.saturating_sub(1));
        }
        self.scroll = self.scroll.min(self.rows.saturating_sub(self.height));
    }
}
