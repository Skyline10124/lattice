use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use lattice_core::types::ToolDefinition;
use lattice_plugin::registry::PluginRegistry;

#[derive(Debug, Clone)]
pub(super) struct PreparedSubmission {
    pub(super) display: String,
    pub(super) prompt: String,
    pub(super) raw_prompt: Option<String>,
    pub(super) plugin: Option<RunPluginContext>,
}

#[allow(dead_code)] // Phase 3 owns final plugin command UX cleanup.
#[derive(Debug, Clone)]
pub(super) struct RunPluginContext {
    pub(super) name: String,
    pub(super) system_prompt: String,
    pub(super) output_contract_delta: Option<String>,
    pub(super) tools: Vec<ToolDefinition>,
}

pub(super) fn plugin_count(registry: Option<&Arc<PluginRegistry>>) -> usize {
    registry.map_or(0, |registry| registry.len())
}

pub(super) fn plugin_names(registry: Option<&Arc<PluginRegistry>>) -> Vec<String> {
    registry
        .map(|registry| crate::plugins::sorted_plugin_names(registry))
        .unwrap_or_default()
}

pub(super) fn has_plugin(registry: Option<&Arc<PluginRegistry>>, name: &str) -> bool {
    find_plugin_name(plugin_names(registry), name).is_some()
}

pub(super) fn plugin_summary(
    registry: Option<&Arc<PluginRegistry>>,
    name: &str,
) -> Option<(String, String)> {
    let registry = registry?;
    let canonical = find_plugin_name(plugin_names(Some(registry)), name)?;
    let bundle = registry.get(&canonical)?;
    Some((bundle.meta.name.clone(), bundle.meta.description.clone()))
}

pub(super) fn prepare_submission(
    registry: Option<&Arc<PluginRegistry>>,
    workdir: &Path,
    name: &str,
    prompt: String,
) -> Result<PreparedSubmission> {
    let registry = registry.ok_or_else(|| anyhow::anyhow!("plugin registry is not initialized"))?;
    let canonical = find_plugin_name(plugin_names(Some(registry)), name)
        .ok_or_else(|| anyhow::anyhow!("plugin '{name}' is not loaded"))?;
    let bundle = registry
        .get(&canonical)
        .ok_or_else(|| anyhow::anyhow!("plugin '{name}' is not loaded"))?;
    let plugin_prompt = bundle
        .plugin
        .to_prompt_json(&plugin_context(&bundle.meta.name, &prompt, workdir))
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    let display = format!("@{} {}", bundle.meta.name, prompt);

    Ok(PreparedSubmission {
        display,
        prompt: plugin_prompt,
        raw_prompt: Some(prompt),
        plugin: Some(RunPluginContext {
            name: bundle.meta.name.clone(),
            system_prompt: bundle.plugin.system_prompt().to_string(),
            output_contract_delta: output_contract_delta(bundle.plugin.output_schema()),
            tools: bundle
                .plugin
                .tools()
                .iter()
                .cloned()
                .chain(bundle.default_tools.iter().cloned())
                .collect(),
        }),
    })
}

fn find_plugin_name(names: Vec<String>, query: &str) -> Option<String> {
    let query = query.trim();
    names
        .into_iter()
        .find(|name| name == query || name.eq_ignore_ascii_case(query))
}

fn plugin_context(name: &str, prompt: &str, workdir: &Path) -> serde_json::Value {
    match canonical_plugin_name(name).as_str() {
        "CodeReview" | "code-review" => serde_json::json!({
            "input": prompt,
            "file_path": "",
            "context_rules": []
        }),
        "Refactor" | "refactor" => serde_json::json!({
            "code": prompt,
            "review": null,
            "instructions": prompt
        }),
        "TestGen" | "test-gen" => serde_json::json!({
            "code": prompt,
            "language": "",
            "focus_areas": []
        }),
        "SecurityAudit" | "security-audit" => serde_json::json!({
            "code": prompt,
            "dependencies": [],
            "threat_model": ""
        }),
        "DocGen" | "doc-gen" => serde_json::json!({
            "code": prompt,
            "doc_type": "technical",
            "audience": "developers"
        }),
        "PlanGen" | "plan-gen" => serde_json::json!({
            "spec": prompt,
            "project_path": workdir.display().to_string(),
            "context_rules": []
        }),
        "DeepResearch" | "deep-research" => serde_json::json!({
            "query": prompt,
            "sources": [],
            "depth": "standard"
        }),
        "ImageGen" | "image-gen" => serde_json::json!({
            "prompt": prompt,
            "style": "",
            "dimensions": ""
        }),
        "KnowledgeBase" | "knowledge-base" => serde_json::json!({
            "query": prompt,
            "kb_sources": []
        }),
        "PptxGen" | "pptx-gen" => serde_json::json!({
            "topic": prompt,
            "outline": [],
            "template": ""
        }),
        "Verification" | "verification" => serde_json::json!({
            "changes": [],
            "plan_task_id": 0,
            "verification_steps": [prompt]
        }),
        _ => serde_json::json!({ "input": prompt, "request": prompt }),
    }
}

fn canonical_plugin_name(name: &str) -> String {
    name.trim().to_string()
}

fn output_contract_delta(schema: Option<serde_json::Value>) -> Option<String> {
    let schema = schema?;
    let output_schema =
        serde_json::to_string_pretty(&schema).unwrap_or_else(|_| schema.to_string());
    Some(format!(
        "Output contract:\nReturn only valid JSON matching this schema:\n{output_schema}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_matching_is_case_insensitive() {
        let found = find_plugin_name(vec!["CodeReview".into(), "DocGen".into()], "codereview");

        assert_eq!(found, Some("CodeReview".into()));
    }

    #[test]
    fn plugin_context_uses_specific_shape_for_plan_gen() {
        let workdir = Path::new("/tmp/project");
        let value = plugin_context("PlanGen", "ship it", workdir);

        assert_eq!(value["spec"], "ship it");
        assert_eq!(value["project_path"], "/tmp/project");
        assert!(value["context_rules"].as_array().unwrap().is_empty());
    }

    #[test]
    fn output_contract_delta_embeds_schema() {
        let delta = output_contract_delta(Some(serde_json::json!({
            "type": "object",
            "required": ["ok"]
        })))
        .unwrap();

        assert!(delta.contains("Output contract:"));
        assert!(delta.contains("\"required\""));
    }

    #[test]
    fn run_plugin_context_keeps_prompt_and_tool_delta_locally() {
        let context = RunPluginContext {
            name: "CodeReview".into(),
            system_prompt: "Review code carefully.".into(),
            output_contract_delta: Some("Return JSON.".into()),
            tools: vec![ToolDefinition::new(
                "plugin_read".into(),
                "Read plugin data".into(),
                serde_json::json!({"type": "object"}),
            )],
        };

        assert_eq!(context.name, "CodeReview");
        assert_eq!(context.system_prompt, "Review code carefully.");
        assert_eq!(
            context.output_contract_delta.as_deref(),
            Some("Return JSON.")
        );
        assert_eq!(context.tools[0].name, "plugin_read");
    }
}
