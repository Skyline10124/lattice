use serde::{Deserialize, Serialize};

/// Sandbox policy for YOLO mode.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum YoloSandboxPolicy {
    /// Force the command allowlist; callers may fill safe defaults when empty.
    #[default]
    EnforceCommandAllowlist,
    /// Disable bash-style command execution.
    NoBash,
    /// Use an explicit command allowlist.
    Custom { allowed_commands: Vec<String> },
}

/// Runtime behavior mode shared by agents, plugin slots, and profiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BehaviorMode {
    Strict {
        #[serde(default = "default_confidence")]
        confidence_threshold: f64,
        #[serde(default = "default_max_retries")]
        max_retries: u32,
        #[serde(default)]
        escalate_to: Option<String>,
    },
    Yolo {
        #[serde(default)]
        enforce_sandbox: YoloSandboxPolicy,
    },
}

fn default_confidence() -> f64 {
    0.7
}

fn default_max_retries() -> u32 {
    3
}

impl Default for BehaviorMode {
    fn default() -> Self {
        BehaviorMode::Strict {
            confidence_threshold: default_confidence(),
            max_retries: default_max_retries(),
            escalate_to: None,
        }
    }
}

impl BehaviorMode {
    pub fn is_yolo(&self) -> bool {
        matches!(self, BehaviorMode::Yolo { .. })
    }

    pub fn is_strict(&self) -> bool {
        matches!(self, BehaviorMode::Strict { .. })
    }
}
