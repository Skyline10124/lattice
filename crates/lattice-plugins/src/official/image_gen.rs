use serde::{Deserialize, Serialize};

use lattice_plugin::Plugin;

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

impl Default for ImageGenPlugin {
    fn default() -> Self {
        Self::new()
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
}
