// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Subcommand;
use lattice_local::{
    Availability, InputModality, LocalClient, ModelCandidateQuery, ProviderId, StateLayout,
};
use std::io::Write;

#[derive(Subcommand)]
pub enum ModelsAction {
    #[command(about = "Refresh model metadata and the signed-in ChatGPT account catalog.")]
    Refresh,
}

pub async fn run(
    provider: Option<ProviderId>,
    auth_only: bool,
    action: Option<ModelsAction>,
    json: bool,
) -> Result<()> {
    let client = LocalClient::new(StateLayout::resolve()?);
    client.status().await?;
    if let Some(ModelsAction::Refresh) = action {
        return refresh(&client, json, &mut std::io::stdout()).await;
    }
    let candidates = client
        .models(ModelCandidateQuery::List {
            provider,
            ready_only: auth_only,
        })
        .await?;

    if json {
        println!("{}", serde_json::to_string(&candidates)?);
        return Ok(());
    }

    println!(
        "MODEL\tCONTEXT\tOUTPUT\tEFFORTS\tINPUT\tPRICE ($/M tokens: input/output/cache-read/cache-write)\tAUTH"
    );
    let mut config_errors = Vec::new();
    for model in candidates.models {
        let limits =
            |value: Option<u64>| value.map_or_else(|| "-".into(), |value| value.to_string());
        let price =
            |value: Option<f64>| value.map_or_else(|| "-".into(), |value| value.to_string());
        let cost = model.cost.unwrap_or_default();
        let auth_status = model.availability.status_str();
        // 配置无效的模型仍列出，档位列标记出错，诊断随警告写到 stderr。
        let efforts = if let Availability::Invalid { reason } = model.availability {
            config_errors.push((format!("{}/{}", model.provider, model.model), reason));
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
        let input = format_input_modalities(model.input.as_deref());
        println!(
            "{}/{}\t{}\t{}\t{}\t{}\t{}/{}/{}/{}\t{}",
            model.provider,
            model.model,
            limits(model.context),
            limits(model.output),
            efforts,
            input,
            price(cost.input),
            price(cost.output),
            price(cost.cache_read),
            price(cost.cache_write),
            auth_status
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

fn format_input_modalities(input: Option<&[InputModality]>) -> String {
    match input {
        Some(modalities) if !modalities.is_empty() => modalities
            .iter()
            .map(|modality| modality.as_str())
            .collect::<Vec<_>>()
            .join(","),
        _ => "-".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_input_modalities_covers_non_empty_empty_and_none_cells() {
        assert_eq!(
            format_input_modalities(Some(&[InputModality::Text, InputModality::Image])),
            "text,image",
            "非空输入模态列表应用逗号连接标签"
        );
        assert_eq!(
            format_input_modalities(Some(&[
                InputModality::Text,
                InputModality::Image,
                InputModality::Audio,
                InputModality::Video,
            ])),
            "text,image,audio,video",
            "全部四种输入模态应按原顺序连接"
        );
        assert_eq!(
            format_input_modalities(Some(&[])),
            "-",
            "空输入模态列表应显示连字符占位"
        );
        assert_eq!(
            format_input_modalities(None),
            "-",
            "缺失输入模态元数据应显示连字符占位"
        );
    }
}
