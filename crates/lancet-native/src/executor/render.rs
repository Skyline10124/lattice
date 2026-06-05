// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub(crate) fn render_file_slice(
    content: &str,
    offset: usize,
    limit: usize,
    line_numbers: bool,
) -> String {
    let total_lines = content.lines().count();
    let start_index = offset.saturating_sub(1);
    if start_index >= total_lines {
        return format!(
            "[No content: requested offset {} but file has {} lines]",
            offset, total_lines
        );
    }

    let mut rendered = Vec::new();
    let mut returned = 0usize;
    for (line_index, line) in content.lines().enumerate().skip(start_index) {
        if returned >= limit {
            break;
        }
        if line_numbers {
            rendered.push(format!("{:>6}\t{}", line_index + 1, line));
        } else {
            rendered.push(line.to_string());
        }
        returned += 1;
    }

    if start_index + returned < total_lines {
        rendered.push(format!(
            "[TRUNCATED: showing lines {}-{} of {}; call read_file with offset {} to continue]",
            offset,
            start_index + returned,
            total_lines,
            start_index + returned + 1
        ));
    }

    rendered.join("\n")
}
