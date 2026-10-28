// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use lattice_api::{
    StateLayout,
    local::{AuthState, LocalClient},
};

pub async fn run(provider: Option<String>, auth_only: bool, json: bool) -> Result<()> {
    let client = LocalClient::new(StateLayout::resolve()?);
    client.ensure_running().await?;
    let mut candidates = client.models(provider).await?;
    if auth_only {
        candidates
            .models
            .retain(|model| model.credential_status == AuthState::Ready);
    }

    if json {
        println!("{}", serde_json::to_string(&candidates)?);
        return Ok(());
    }

    println!(
        "MODEL\tCONTEXT\tOUTPUT\tPRICE ($/M tokens: input/output/cache-read/cache-write/reasoning)\tAUTH"
    );
    for model in candidates.models {
        let limits =
            |value: Option<u64>| value.map_or_else(|| "-".into(), |value| value.to_string());
        let price =
            |value: Option<f64>| value.map_or_else(|| "-".into(), |value| value.to_string());
        let cost = model.cost.unwrap_or_default();
        println!(
            "{}/{}\t{}\t{}\t{}/{}/{}/{}/{}\t{}",
            model.provider,
            model.model,
            limits(model.context),
            limits(model.output),
            price(cost.input),
            price(cost.output),
            price(cost.cache_read),
            price(cost.cache_write),
            price(cost.reasoning),
            model.credential_status
        );
    }
    for warning in candidates.warnings {
        if let Some(provider) = warning.provider {
            eprintln!("{provider}: {}", warning.message);
        } else {
            eprintln!("{}", warning.message);
        }
    }

    Ok(())
}
