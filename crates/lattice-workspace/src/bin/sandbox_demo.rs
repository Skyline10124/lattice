// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Drives the SAME scenario through both sandbox backends via the single
//! `Workspace` trait, proving the adapter seam. Run it against the live local
//! deployments:
//!
//! ```sh
//! # microsandbox needs its state dir; cube exec (cubecli) needs root.
//! sudo -E env HOME="$HOME" \
//!   CUBE_TEMPLATE_ID=tpl-962da20ccb2b41f392b0f760 \
//!   cargo run -p lattice-workspace --bin sandbox-demo
//! ```

use std::time::Instant;

use lattice_workspace::{
    SandboxSpec, Workspace, cube::CubeWorkspace, microsandbox::MicrosandboxWorkspace,
};

fn ms(t: Instant) -> u128 {
    t.elapsed().as_millis()
}

async fn run_scenario(ws: Box<dyn Workspace>, image: &str) {
    let caps = ws.capabilities();
    println!("\n========== backend: {} ==========", ws.backend());
    println!(
        "capabilities: exec={} stdout_capture={} disk_snapshot={} memory_snapshot={} needs_root_exec={}",
        caps.exec,
        caps.stdout_capture,
        caps.disk_snapshot,
        caps.memory_snapshot,
        caps.needs_root_exec
    );

    let spec = SandboxSpec {
        image: image.to_string(),
        timeout_secs: 120,
    };

    // --- create ---
    let t = Instant::now();
    let handle = match ws.create(&spec).await {
        Ok(h) => {
            println!("create   : ok  id={}  ({} ms)", h.id, ms(t));
            h
        }
        Err(e) => {
            println!("create   : FAILED: {e}");
            return;
        }
    };

    // --- exec: a probe whose exit code is meaningful on every backend ---
    let t = Instant::now();
    match ws.exec(&handle, &["python3", "--version"]).await {
        Ok(o) => {
            let shown = if o.stdout.trim().is_empty() {
                "(stdout not captured by this backend)".to_string()
            } else {
                o.stdout.trim().to_string()
            };
            println!(
                "exec     : exit={} stdout={:?}  ({} ms)",
                o.exit_code,
                shown,
                ms(t)
            );
        }
        Err(e) => println!("exec     : FAILED: {e}"),
    }

    // --- exec: shows output capture difference between backends ---
    let t = Instant::now();
    match ws
        .exec(&handle, &["sh", "-c", "echo hello-from-$(uname -m)"])
        .await
    {
        Ok(o) => println!(
            "exec     : exit={} stdout={:?}  ({} ms)",
            o.exit_code,
            o.stdout.trim(),
            ms(t)
        ),
        Err(e) => println!("exec     : FAILED: {e}"),
    }

    // --- snapshot: verify-discard-layer primitive ---
    let t = Instant::now();
    match ws
        .snapshot(&handle, &format!("demo-{}", ws.backend()))
        .await
    {
        Ok(s) => println!("snapshot : ok  ref={s}  ({} ms)", ms(t)),
        Err(e) => println!("snapshot : {e}"),
    }

    // --- destroy ---
    let t = Instant::now();
    match ws.destroy(&handle).await {
        Ok(()) => println!("destroy  : ok  ({} ms)", ms(t)),
        Err(e) => println!("destroy  : FAILED: {e}"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let msb_image = std::env::var("MSB_IMAGE")
        .unwrap_or_else(|_| "docker.m.daocloud.io/library/python:3.12-slim".to_string());
    let cube_tpl = std::env::var("CUBE_TEMPLATE_ID")
        .unwrap_or_else(|_| "tpl-962da20ccb2b41f392b0f760".to_string());

    println!("LATTICE workspace adapter MVP — one trait, two sandbox backends");

    run_scenario(Box::new(MicrosandboxWorkspace::from_env()), &msb_image).await;
    run_scenario(Box::new(CubeWorkspace::from_env()), &cube_tpl).await;

    Ok(())
}
