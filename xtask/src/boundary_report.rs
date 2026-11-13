// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::output::{exit_with_status_on_failures, print_group};

/// 依次运行具名的边界检查组并逐组打印结果；任一组未通过时以退出码 1 结束进程。
pub fn run_groups(
    passed_message: &str,
    failed_message: &str,
    groups: &[(&str, Vec<String>)],
) -> anyhow::Result<()> {
    let mut failures = 0usize;
    for (name, lines) in groups {
        if !print_group(name, lines) {
            failures += 1;
        }
    }
    exit_with_status_on_failures(passed_message, failed_message, failures);
    Ok(())
}
