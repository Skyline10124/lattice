// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;

use anyhow::Context;
use lattice_llm::{MODELS_URL, fetch_snapshot, load_tls_roots_der, usable_tls_roots_der};

pub fn run() -> anyhow::Result<()> {
    // 元数据源是 https：按网关同样的规则选根，一张可用的都没有就在这里报错。
    let roots = usable_tls_roots_der(load_tls_roots_der());
    if roots.is_empty() {
        anyhow::bail!("没有可用的 TLS 根证书，无法拉取 https 元数据源 {MODELS_URL}");
    }
    let runtime = tokio::runtime::Runtime::new().context("创建元数据拉取 runtime")?;
    let snapshot = runtime.block_on(fetch_snapshot(MODELS_URL, &roots))?;
    let path = super::repo_root::repo_root()?.join("crates/lattice-llm/src/metadata/snapshot.json");
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temporary.write_all(&snapshot)?;
    temporary.persist(&path)?;
    println!("已生成 {}（{} 字节）", path.display(), snapshot.len());
    Ok(())
}
