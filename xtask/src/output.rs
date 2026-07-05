// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub fn print_group(group_name: &str, group_output: &[String]) -> bool {
    if group_output.is_empty() {
        println!("PASS [{group_name}]");
        true
    } else {
        println!();
        println!("FAIL [{group_name}]");
        for line in group_output {
            println!("{line}");
        }
        false
    }
}

/// Prints the final boundary summary and **terminates the process** with code 1 when
/// `failures > 0`.
pub fn exit_with_status_on_failures(passed_message: &str, failed_message: &str, failures: usize) {
    if failures > 0 {
        println!();
        println!("{failed_message}: {failures} group(s)");
        std::process::exit(1);
    }
    println!();
    println!("{passed_message}");
}
