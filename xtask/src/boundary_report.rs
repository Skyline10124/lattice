// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::output::{exit_with_status_on_failures, print_group};

/// Run named boundary groups, print PASS/FAIL per group, then exit(1) on any failure.
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
