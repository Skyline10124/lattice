use serde::{Deserialize, Serialize};

use lattice_plugin::Plugin;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocGenInput {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub doc_type: String,
    #[serde(default)]
    pub audience: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocGenOutput {
    #[serde(default)]
    pub documentation: String,
    #[serde(default)]
    pub sections: Vec<String>,
}

pub struct DocGenPlugin;

impl DocGenPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DocGenPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for DocGenPlugin {
    type Input = DocGenInput;
    type Output = DocGenOutput;

    fn name(&self) -> &str {
        "doc-gen"
    }

    fn system_prompt(&self) -> &str {
        "You generate technical documentation. Return ONLY valid JSON with 'documentation' and 'sections'."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Generate {} documentation for {}.\nCODE:\n{}",
            input.doc_type, input.audience, input.code
        )
    }
}
