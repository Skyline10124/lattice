// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-store-probe`: the lightweight qualification binary for a target
//! state location (ADR 0005 §18).
//!
//! Runs mount reporting, the per-location capability probes, and a minimal
//! repository create/publish/reopen smoke through the real kernel
//! first-create path. Build once, copy to the host, run:
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

use lattice_store::meta::StoreScope;
use lattice_store::probe::probe_location_report;
use lattice_store::{StateLayout, StoreMeta, StoreRepo};

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

    // Minimal repository smoke through the real first-create path.
    let smoke = location.join(".lattice-store-probe-smoke.git");
    let smoke_result = (|| -> Result<(), lattice_store::StoreError> {
        let meta = StoreMeta {
            store_kind: "probe".to_string(),
            store_schema_version: 1,
            scope: StoreScope::Project {
                key: lattice_store::ProjectStoreKey::derive(&location)?,
            },
        };
        let repo = StoreRepo::create(&smoke, meta, |repo| {
            let blob = repo.write_blob(b"probe-smoke")?;
            repo.cas_ref(
                "refs/lattice/probe/smoke",
                blob,
                lattice_store::RefExpectation::MustNotExist,
                "probe smoke",
            )?;
            repo.fsync_publication(&[blob], &["refs/lattice/probe/smoke"])
        })?;
        drop(repo);
        let expected = lattice_store::ExpectedMeta {
            store_kind: "probe".to_string(),
            store_schema_version: 1,
            scope: StoreScope::Project {
                key: lattice_store::ProjectStoreKey::derive(&location)?,
            },
        };
        let reopened = StoreRepo::open(&smoke, &expected)?;
        if reopened.ref_target("refs/lattice/probe/smoke")?.is_none() {
            eprintln!("smoke ref missing after reopen");
            std::process::exit(1);
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&smoke);
    // A leftover create lock file is harmless but tidy to remove.
    let _ = std::fs::remove_file(location.join("..lattice-store-probe-smoke.git.create.lock"));

    match smoke_result {
        Ok(()) => {
            println!("  [ok] kernel first-create/reopen smoke");
            println!("verdict: PASS (capabilities verified at this location)");
            ExitCode::SUCCESS
        }
        Err(err) => {
            println!("  [FAIL] kernel first-create/reopen smoke: {err}");
            println!("verdict: FAIL (fail closed)");
            ExitCode::FAILURE
        }
    }
}
