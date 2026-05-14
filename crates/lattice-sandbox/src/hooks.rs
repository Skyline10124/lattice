/// Trait for pre-execute security hooks. Abstracts HookChain from lattice-agent.
pub trait CommandHook: Send + Sync {
    /// Run pre-execute checks. Returns Ok(modified_command) or Err(deny_reason).
    fn run_pre_execute(&self, tool: &str, command: &str) -> Result<String, HookDeny>;
}

pub struct HookDeny {
    pub hook: String,
    pub reason: String,
    pub severity: HookSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HookSeverity {
    Warning,
    Deny,
    Lockdown,
}

impl std::fmt::Display for HookSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookSeverity::Warning => write!(f, "Warning"),
            HookSeverity::Deny => write!(f, "Deny"),
            HookSeverity::Lockdown => write!(f, "Lockdown"),
        }
    }
}
