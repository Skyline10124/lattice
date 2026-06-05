// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolLimits {
    pub max_read_size: usize,
    pub max_write_size: usize,
    pub max_http_response_size: usize,
    pub max_command_timeout: u32,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_read_size: 10 * 1024 * 1024,
            max_write_size: 1024 * 1024,
            max_http_response_size: 10 * 1024 * 1024,
            max_command_timeout: 30,
        }
    }
}
