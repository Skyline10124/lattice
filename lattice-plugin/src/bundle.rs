use serde::{Deserialize, Serialize};

use lattice_core::types::ToolDefinition;

use crate::erased::ErasedPlugin;
use crate::{Behavior, StrictBehavior, YoloBehavior};

/// Plugin metadata for registry listing and discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
}

/// Configurable behavior mode — maps to Behavior trait at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BehaviorMode {
    Strict {
        confidence_threshold: f64,
        max_retries: u32,
        escalate_to: Option<String>,
    },
    Yolo,
}

impl BehaviorMode {
    pub fn to_behavior(&self) -> Box<dyn Behavior> {
        match self.clone() {
            BehaviorMode::Strict {
                confidence_threshold,
                max_retries,
                escalate_to,
            } => Box::new(StrictBehavior {
                confidence_threshold,
                max_retries,
                escalate_to,
            }),
            BehaviorMode::Yolo => Box::new(YoloBehavior),
        }
    }
}

/// A registered plugin with metadata, default behavior, and default tools.
pub struct PluginBundle {
    pub meta: PluginMeta,
    pub plugin: Box<dyn ErasedPlugin>,
    pub default_behavior: BehaviorMode,
    pub default_tools: Vec<ToolDefinition>,
}
