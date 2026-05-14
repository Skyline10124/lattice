use serde::{Deserialize, Serialize};

use lattice_plugin::Plugin;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseInput {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub kb_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseResult {
    pub title: String,
    pub snippet: String,
    pub relevance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseOutput {
    #[serde(default)]
    pub results: Vec<KnowledgeBaseResult>,
    #[serde(default)]
    pub relevance_scores: Vec<f64>,
}

pub struct KnowledgeBasePlugin;

impl KnowledgeBasePlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for KnowledgeBasePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for KnowledgeBasePlugin {
    type Input = KnowledgeBaseInput;
    type Output = KnowledgeBaseOutput;

    fn name(&self) -> &str {
        "knowledge-base"
    }

    fn system_prompt(&self) -> &str {
        "You query knowledge bases. Synthesize results. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Query: {}\nSources:\n{}",
            input.query,
            input.kb_sources.join("\n")
        )
    }
}
