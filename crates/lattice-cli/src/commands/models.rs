// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;

/// List known model aliases.
///
/// Plain `models` lists all known aliases one per line.
/// `models --auth` lists only aliases whose credentials are present.
pub fn run(auth_only: bool, json: bool) -> Result<()> {
    let entries = lattice_api::list_models(auth_only);

    if json {
        let items: Vec<serde_json::Value> = entries
            .iter()
            .map(super::model_render::resolved_model_json)
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    for entry in &entries {
        println!("{}", entry.alias);
    }

    Ok(())
}
