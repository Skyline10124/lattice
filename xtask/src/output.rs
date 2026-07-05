// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub fn print_group(group_name: &str, group_output: &str) -> bool {
    if group_output.is_empty() {
        println!("PASS [{group_name}]");
        true
    } else {
        println!();
        println!("FAIL [{group_name}]");
        println!("{group_output}");
        false
    }
}

pub fn finish_banner(passed_message: &str, failed_message: &str, failures: usize) {
    if failures > 0 {
        println!();
        println!("{failed_message}: {failures} group(s)");
        std::process::exit(1);
    }
    println!();
    println!("{passed_message}");
}

pub fn repo_root() -> anyhow::Result<std::path::PathBuf> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask manifest has no parent directory"))?
        .to_path_buf())
}
