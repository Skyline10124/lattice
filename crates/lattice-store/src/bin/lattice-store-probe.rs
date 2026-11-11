// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-store-probe`: the lightweight qualification binary for a target
//! state location (ADR 0005 §18).
//!
//! 报告挂载信息，并在指定位置执行文件系统能力探测。构建一次后复制到目标主机运行：
//!
//! ```text
//! lattice-store-probe [STATE_ROOT_OR_DIR]
//! ```
//!
//! With no argument the XDG state root's `lattice/` directory is probed.
//! Exit code 0 means every capability is present at that location; anything
//! else is a fail-closed report. A passing non-gate filesystem qualifies as
//! `Unverified`; only maintained Btrfs is the full gate.

use std::path::PathBuf;
use std::process::ExitCode;

use lattice_store::StateLayout;
use lattice_store::probe::probe_location_report;

fn main() -> ExitCode {
    let location = match std::env::args().nth(1) {
        Some(arg) => PathBuf::from(arg),
        None => match StateLayout::resolve() {
            Ok(layout) => layout.lattice_root().to_path_buf(),
            Err(err) => {
                eprintln!("cannot resolve XDG state root: {err}");
                return ExitCode::from(2);
            }
        },
    };

    println!("lattice-store qualification probe");
    println!("location: {}", location.display());

    let report = probe_location_report(&location);
    match &report.mount {
        Some(mount) => println!("mount: {} (fstype {})", mount.source, mount.fstype),
        None => println!("mount: <not found in /proc/self/mounts>"),
    }
    for check in &report.checks {
        println!(
            "  [{}] {} — {}",
            if check.ok { "ok" } else { "FAIL" },
            check.name,
            check.detail
        );
    }
    if !report.all_ok() {
        println!("verdict: MISSING PRIMITIVES (fail closed)");
        return ExitCode::FAILURE;
    }

    println!("verdict: PASS (capabilities verified at this location)");
    ExitCode::SUCCESS
}
