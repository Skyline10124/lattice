// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Subcommand;
use lattice_api::{
    StateLayout,
    local::{AuthState, LocalClient},
};
use std::io::Write;

#[derive(Subcommand)]
pub enum ModelsAction {
    /// Refresh model metadata from models.dev.
    Refresh,
}

pub async fn run(
    provider: Option<String>,
    auth_only: bool,
    action: Option<ModelsAction>,
    json: bool,
) -> Result<()> {
    let client = LocalClient::new(StateLayout::resolve()?);
    client.ensure_running().await?;
    if let Some(ModelsAction::Refresh) = action {
        return refresh(&client, json, &mut std::io::stdout()).await;
    }
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
        "MODEL\tCONTEXT\tOUTPUT\tEFFORTS\tPRICE ($/M tokens: input/output/cache-read/cache-write)\tAUTH"
    );
    let mut config_errors = Vec::new();
    for model in candidates.models {
        let limits =
            |value: Option<u64>| value.map_or_else(|| "-".into(), |value| value.to_string());
        let price =
            |value: Option<f64>| value.map_or_else(|| "-".into(), |value| value.to_string());
        let cost = model.cost.unwrap_or_default();
        // 配置无效的模型仍列出，档位列标记出错，诊断随警告写到 stderr。
        let efforts = if let Some(error) = model.config_error {
            config_errors.push((format!("{}/{}", model.provider, model.model), error));
            "config-error".into()
        } else if model.supported_efforts.is_empty() {
            "-".into()
        } else {
            model
                .supported_efforts
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        println!(
            "{}/{}\t{}\t{}\t{}\t{}/{}/{}/{}\t{}",
            model.provider,
            model.model,
            limits(model.context),
            limits(model.output),
            efforts,
            price(cost.input),
            price(cost.output),
            price(cost.cache_read),
            price(cost.cache_write),
            model.credential_status
        );
    }
    for (model, error) in config_errors {
        eprintln!("{model}: {error}");
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

pub(super) async fn refresh(client: &LocalClient, json: bool, out: &mut impl Write) -> Result<()> {
    client.models_refresh().await?;
    if json {
        writeln!(
            out,
            "{}",
            serde_json::json!({"type":"models_refresh","status":"refreshed"})
        )?;
    } else {
        writeln!(out, "Model metadata refreshed.")?;
    }
    Ok(())
}
