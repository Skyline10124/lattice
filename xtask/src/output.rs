// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub fn print_group(group_name: &str, group_output: &[String]) -> bool {
    if group_output.is_empty() {
        println!("通过 [{group_name}]");
        true
    } else {
        println!();
        println!("失败 [{group_name}]");
        for line in group_output {
            println!("{line}");
        }
        false
    }
}

/// 打印边界检查的最终总结；`failures > 0` 时以退出码 1 **直接结束进程**。
pub fn exit_with_status_on_failures(passed_message: &str, failed_message: &str, failures: usize) {
    if failures > 0 {
        println!();
        println!("{failed_message}：{failures} 组未通过");
        std::process::exit(1);
    }
    println!();
    println!("{passed_message}");
}
