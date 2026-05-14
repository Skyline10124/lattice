pub mod approval;
pub mod audit;
pub mod checks;
pub mod command_utils;
pub mod config;
pub mod denial;
pub mod error;
pub mod hooks;
pub mod path_utils;
pub mod security;

#[cfg(target_os = "linux")]
pub mod landlock;

#[cfg(unix)]
pub mod limits;

// ---------------------------------------------------------------------------
// Re-exports from lattice-policy (canonical home for policy types)
// ---------------------------------------------------------------------------

pub use lattice_policy::{
    approval_kind_from_approve_command, command_matches_allowlist, command_matches_exact,
    dangerous_command_pattern, is_path_prefix, path_matches_exact, path_matches_recursive,
    validate_command_exact_rule, validate_command_prefix_rule, validate_command_rule,
    validate_path_rule, ApprovalGrant, ApprovalKind, ApprovalProposal, ApprovalProposalKind,
    DenialKind, DenialReport, PathOperation, PolicyAuthority, PolicyDecision, RiskClass,
    ToolCatalog, ValidationError, DANGEROUS_COMMAND_PATTERNS,
};

/// Compatibility alias: `ApprovedRule` is now [`ApprovalGrant`].
pub type ApprovedRule = ApprovalGrant;

pub use audit::{AuditEventKind, AuditSink};
pub use config::{LandlockConfig, SandboxConfig};
pub use error::SandboxError;
pub use hooks::{CommandHook, HookDeny, HookSeverity};

#[cfg(test)]
mod tests;
