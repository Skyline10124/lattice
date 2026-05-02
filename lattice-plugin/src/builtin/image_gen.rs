use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenInput {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub dimensions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenOutput {
    #[serde(default)]
    pub image_prompt: String,
    #[serde(default)]
    pub alt_text: String,
    #[serde(default)]
    pub metadata: String,
}

pub struct ImageGenPlugin;

impl ImageGenPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for ImageGenPlugin {
    type Input = ImageGenInput;
    type Output = ImageGenOutput;

    fn name(&self) -> &str {
        "image-gen"
    }

    fn system_prompt(&self) -> &str {
        "You craft detailed image generation prompts. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Create an image prompt.\nDescription: {}\nStyle: {}\nDimensions: {}",
            input.prompt, input.style, input.dimensions
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
