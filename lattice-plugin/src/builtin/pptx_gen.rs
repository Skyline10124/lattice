use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PptxGenInput {
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub outline: Vec<String>,
    #[serde(default)]
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slide {
    pub title: String,
    pub bullets: Vec<String>,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PptxGenOutput {
    #[serde(default)]
    pub slides: Vec<Slide>,
    #[serde(default)]
    pub speaker_notes: String,
}

pub struct PptxGenPlugin;

impl PptxGenPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for PptxGenPlugin {
    type Input = PptxGenInput;
    type Output = PptxGenOutput;

    fn name(&self) -> &str {
        "pptx-gen"
    }

    fn system_prompt(&self) -> &str {
        "You generate PowerPoint presentations. Return ONLY valid JSON with 'slides' and 'speaker_notes'."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Create a presentation about: {}\nOutline: {}\nTemplate: {}",
            input.topic,
            input.outline.join(", "),
            input.template
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
