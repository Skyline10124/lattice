use serde::{Deserialize, Serialize};

use lattice_core::types::ToolDefinition;

use crate::erased::ErasedPlugin;
use crate::{Behavior, StrictBehavior, YoloBehavior};

pub use lattice_core::types::{BehaviorMode, YoloSandboxPolicy};

/// Plugin metadata for registry listing and discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
}

/// Convert a BehaviorMode to the corresponding Behavior trait object at runtime.
pub fn behavior_to_behavior_trait(mode: &BehaviorMode) -> Box<dyn Behavior> {
    match mode.clone() {
        BehaviorMode::Strict {
            confidence_threshold,
            max_retries,
            escalate_to,
        } => Box::new(StrictBehavior {
            confidence_threshold,
            max_retries,
            escalate_to,
        }),
        BehaviorMode::Yolo { .. } => {
            tracing::warn!(
                "YOLO mode enabled — agent will execute tool calls without output validation"
            );
            Box::new(YoloBehavior)
        }
    }
}

/// Compute the effective sandbox configuration based on behavior mode.
///
/// For Strict mode the base sandbox is returned unchanged.
/// For YOLO mode, the `enforce_sandbox` policy is applied on top of base.
pub fn effective_sandbox(
    behavior: &BehaviorMode,
    base: &lattice_sandbox::SandboxConfig,
) -> lattice_sandbox::SandboxConfig {
    match behavior {
        BehaviorMode::Strict { .. } => base.clone(),
        BehaviorMode::Yolo { enforce_sandbox } => {
            let mut s = base.clone();
            match enforce_sandbox {
                YoloSandboxPolicy::EnforceCommandAllowlist => {
                    if s.command_allowlist.is_empty() {
                        s.command_allowlist =
                            vec!["grep".into(), "find".into(), "ls".into(), "ps".into()];
                    }
                }
                YoloSandboxPolicy::NoBash => {
                    // Caller should handle — NoBash means disable the bash tool entirely.
                    // We set an empty allowlist to let the caller distinguish this case.
                    s.command_allowlist = vec![];
                }
                YoloSandboxPolicy::Custom { allowed_commands } => {
                    s.command_allowlist = allowed_commands.clone();
                }
            }
            s
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
