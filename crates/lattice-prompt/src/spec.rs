use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::system_stack::TRUSTED_SYSTEM_VARIABLES;
use crate::template::{render_template, TemplateError};
use crate::types::{
    CacheScope, ContextSource, Layer, PromptAuthority, PromptChannel, PromptContribution,
    TokenBudget,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSpec {
    #[serde(default = "default_spec_version")]
    pub version: u32,
    /// Top-level system prompt template (D-03).
    /// Can reference `{{system_prompt}}` to embed old system.prompt content.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
    #[serde(default)]
    pub sections: Vec<PromptSectionSpec>,
    #[serde(default)]
    pub providers: Vec<PromptProviderSpec>,
    #[serde(default)]
    pub hooks: Vec<PromptHookSpec>,
}

impl Default for PromptSpec {
    fn default() -> Self {
        Self {
            version: default_spec_version(),
            content: None,
            variables: HashMap::new(),
            sections: Vec::new(),
            providers: Vec::new(),
            hooks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSectionSpec {
    pub id: String,
    pub layer: Layer,
    #[serde(default)]
    pub channel: Option<PromptChannel>,
    #[serde(default)]
    pub priority: u8,
    #[serde(default)]
    pub cache_scope: Option<CacheScope>,
    #[serde(default)]
    pub authority: Option<PromptAuthority>,
    #[serde(default)]
    pub source: Option<ContextSource>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub budget: BudgetSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    ProjectInstructions,
    Memory,
    Events,
    ToolSchema,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptProviderSpec {
    pub id: String,
    pub kind: ProviderKind,
    pub layer: Layer,
    #[serde(default)]
    pub channel: Option<PromptChannel>,
    #[serde(default)]
    pub priority: u8,
    #[serde(default)]
    pub cache_scope: Option<CacheScope>,
    #[serde(default)]
    pub budget: BudgetSpec,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptHookSpec {
    pub id: String,
    pub point: String,
    pub action: String,
    #[serde(default)]
    pub target: Option<PromptTargetSpec>,
    #[serde(default)]
    pub budget: Option<BudgetSpec>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTargetSpec {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub layer: Option<Layer>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BudgetSpec {
    Fixed {
        #[serde(default)]
        tokens: Option<u32>,
    },
    Ratio {
        value: f64,
    },
    #[default]
    Dynamic,
}

impl BudgetSpec {
    pub fn to_token_budget(self, fallback_tokens: u32) -> TokenBudget {
        match self {
            BudgetSpec::Fixed { tokens } => TokenBudget::Fixed(tokens.unwrap_or(fallback_tokens)),
            BudgetSpec::Ratio { value } => TokenBudget::Ratio(value),
            BudgetSpec::Dynamic => TokenBudget::Dynamic,
        }
    }
}

impl PromptSpec {
    /// Returns true if the spec declares a provider of the given kind.
    pub fn has_provider_kind(&self, kind: ProviderKind) -> bool {
        self.providers.iter().any(|p| p.kind == kind)
    }

    pub fn validate(&self) -> Result<(), PromptSpecError> {
        if self.version != 1 {
            return Err(PromptSpecError::UnsupportedVersion(self.version));
        }
        for section in &self.sections {
            section.validate()?;
        }
        for provider in &self.providers {
            let expected_layer = match provider.kind {
                ProviderKind::ProjectInstructions => Layer::Rules,
                ProviderKind::Memory => Layer::Memory,
                ProviderKind::Events => Layer::Events,
                ProviderKind::ToolSchema => Layer::Tools,
            };
            if provider.layer != expected_layer {
                return Err(PromptSpecError::ProviderLayerMismatch {
                    id: provider.id.clone(),
                    kind: format!("{:?}", provider.kind),
                    expected: format!("{:?}", expected_layer),
                    actual: format!("{:?}", provider.layer),
                });
            }
        }
        Ok(())
    }

    pub fn static_contributions(&self) -> Result<Vec<PromptContribution>, PromptSpecError> {
        self.validate()?;
        self.sections
            .iter()
            .map(|section| section.to_contribution(&self.variables))
            .collect()
    }

    /// Render the top-level content field as a System-layer contribution.
    /// The {{system_prompt}} variable bridges old system.prompt content (D-03).
    pub fn content_contribution(
        &self,
        runtime_vars: &HashMap<String, String>,
    ) -> Result<Option<PromptContribution>, PromptSpecError> {
        let Some(ref content_template) = self.content else {
            return Ok(None);
        };
        let (merged, allowed) = merge_variables(&self.variables, runtime_vars);
        let rendered = render_template(
            content_template,
            &merged,
            &allowed.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        )
        .map_err(PromptSpecError::Template)?;
        let tokens = lattice_core::tokens::TokenEstimator::estimate_text(&rendered);
        Ok(Some(PromptContribution::new(
            "prompt-spec.content",
            rendered,
            Layer::System,
            0,
            tokens,
            TokenBudget::Fixed(tokens),
            "prompt-spec",
        )))
    }
}

/// Merge spec variables with runtime variables for template rendering.
/// spec.variables takes precedence over runtime variables (D-14).
pub fn merge_variables(
    spec_vars: &HashMap<String, String>,
    runtime_vars: &HashMap<String, String>,
) -> (HashMap<String, String>, Vec<String>) {
    let mut merged = runtime_vars.clone();
    merged.extend(spec_vars.clone()); // spec overrides runtime (D-14)
    let mut allowed: Vec<String> = TRUSTED_SYSTEM_VARIABLES
        .iter()
        .map(|s| s.to_string())
        .collect();
    allowed.extend(spec_vars.keys().cloned());
    allowed.extend(runtime_vars.keys().cloned());
    (merged, allowed)
}

impl PromptSectionSpec {
    fn validate(&self) -> Result<(), PromptSpecError> {
        if self.id.trim().is_empty() {
            return Err(PromptSpecError::MissingId);
        }

        let sources = [self.content.is_some(), self.template.is_some()]
            .into_iter()
            .filter(|present| *present)
            .count();
        if sources != 1 {
            return Err(PromptSpecError::InvalidSectionSource {
                id: self.id.clone(),
            });
        }
        Ok(())
    }

    pub fn to_contribution(
        &self,
        variables: &HashMap<String, String>,
    ) -> Result<PromptContribution, PromptSpecError> {
        self.validate()?;
        let content = match (&self.content, &self.template) {
            (Some(content), None) => content.clone(),
            (None, Some(template)) => {
                render_template(template, variables, &variable_names(variables))
                    .map_err(PromptSpecError::Template)?
            }
            _ => unreachable!("validated exactly one section source"),
        };
        let tokens = lattice_core::tokens::TokenEstimator::estimate_text(&content);
        let mut contribution = PromptContribution::new(
            self.id.clone(),
            content,
            self.layer,
            self.priority,
            tokens,
            self.budget.to_token_budget(tokens),
            "prompt-spec",
        );
        if let Some(channel) = self.channel {
            contribution.channel = channel;
        }
        if let Some(cache_scope) = self.cache_scope {
            contribution.cache_scope = cache_scope;
        }
        if let Some(authority) = self.authority {
            contribution.authority = authority;
        }
        if let Some(source) = self.source {
            contribution.source = source;
        }
        Ok(contribution)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PromptSpecError {
    #[error("unsupported prompt spec version: {0}")]
    UnsupportedVersion(u32),
    #[error("prompt section id cannot be empty")]
    MissingId,
    #[error("prompt section '{id}' must define exactly one of content or template")]
    InvalidSectionSource { id: String },
    #[error("provider '{id}' ({kind}) requires layer {expected}, got {actual}")]
    ProviderLayerMismatch {
        id: String,
        kind: String,
        expected: String,
        actual: String,
    },
    #[error(transparent)]
    Template(#[from] TemplateError),
}

fn default_spec_version() -> u32 {
    1
}

fn variable_names(variables: &HashMap<String, String>) -> Vec<&str> {
    variables.keys().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_section_template_becomes_contribution() {
        let spec = PromptSpec {
            variables: HashMap::from([("role".to_string(), "reviewer".to_string())]),
            sections: vec![PromptSectionSpec {
                id: "agent-role".to_string(),
                layer: Layer::System,
                channel: Some(PromptChannel::System),
                priority: 10,
                cache_scope: Some(CacheScope::Static),
                authority: None,
                source: None,
                content: None,
                template: Some("You are a {{role}}.".to_string()),
                budget: BudgetSpec::Fixed { tokens: None },
            }],
            ..PromptSpec::default()
        };

        let contributions = spec.static_contributions().unwrap();
        assert_eq!(contributions.len(), 1);
        assert_eq!(contributions[0].id, "agent-role");
        assert_eq!(contributions[0].content, "You are a reviewer.");
        assert!(matches!(contributions[0].budget, TokenBudget::Fixed(_)));
    }

    #[test]
    fn invalid_section_source_is_rejected() {
        let spec = PromptSectionSpec {
            id: "bad".to_string(),
            layer: Layer::System,
            channel: None,
            priority: 0,
            cache_scope: None,
            authority: None,
            source: None,
            content: Some("a".to_string()),
            template: Some("b".to_string()),
            budget: BudgetSpec::Dynamic,
        };

        assert!(matches!(
            spec.validate(),
            Err(PromptSpecError::InvalidSectionSource { .. })
        ));
    }

    #[test]
    fn merge_variables_spec_overrides_runtime() {
        let spec_vars = HashMap::from([("role".to_string(), "reviewer".to_string())]);
        let runtime_vars = HashMap::from([("role".to_string(), "user".to_string())]);
        let (merged, _allowed) = merge_variables(&spec_vars, &runtime_vars);
        assert_eq!(merged.get("role").unwrap(), "reviewer");
    }

    #[test]
    fn merge_variables_includes_all_keys() {
        let spec_vars = HashMap::from([("a".to_string(), "1".to_string())]);
        let runtime_vars = HashMap::from([("b".to_string(), "2".to_string())]);
        let (merged, allowed) = merge_variables(&spec_vars, &runtime_vars);
        assert_eq!(merged.len(), 2);
        assert!(merged.contains_key("a"));
        assert!(merged.contains_key("b"));
        assert!(allowed.contains(&"a".to_string()));
        assert!(allowed.contains(&"b".to_string()));
        assert!(allowed.contains(&"cwd".to_string()));
    }

    #[test]
    fn content_contribution_none_when_no_content() {
        let spec = PromptSpec::default();
        let runtime_vars = HashMap::new();
        assert!(spec.content_contribution(&runtime_vars).unwrap().is_none());
    }

    #[test]
    fn content_contribution_renders_template() {
        let spec = PromptSpec {
            content: Some("You are a {{role}}.".to_string()),
            variables: HashMap::from([("role".to_string(), "assistant".to_string())]),
            ..PromptSpec::default()
        };
        let runtime_vars = HashMap::new();
        let contrib = spec.content_contribution(&runtime_vars).unwrap().unwrap();
        assert_eq!(contrib.content, "You are a assistant.");
        assert_eq!(contrib.layer, Layer::System);
        assert_eq!(contrib.id, "prompt-spec.content");
    }

    #[test]
    fn content_contribution_uses_system_prompt_variable() {
        let spec = PromptSpec {
            content: Some("{{system_prompt}}\nExtra instructions.".to_string()),
            ..PromptSpec::default()
        };
        let runtime_vars =
            HashMap::from([("system_prompt".to_string(), "Original prompt.".to_string())]);
        let contrib = spec.content_contribution(&runtime_vars).unwrap().unwrap();
        assert!(contrib.content.contains("Original prompt."));
        assert!(contrib.content.contains("Extra instructions."));
    }
}
