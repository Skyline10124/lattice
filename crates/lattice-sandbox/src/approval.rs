//! Compatibility re-exports — the canonical types live in `lattice-policy`.
//!
//! These re-exports keep `use lattice_sandbox::approval::*` working for
//! existing callers. New code should import from `lattice_policy` directly
//! or use the crate-level `lattice_sandbox::*` re-exports.

pub use lattice_policy::{
    command_matches_allowlist, command_matches_exact, dangerous_command_pattern, is_path_prefix,
    path_matches_exact, path_matches_recursive, validate_command_exact_rule,
    validate_command_prefix_rule, validate_command_rule, validate_path_rule, ApprovalGrant,
    ApprovalKind, ValidationError, BLOCKED_TOP_LEVEL_PATHS, DANGEROUS_COMMAND_PATTERNS,
};

/// Compatibility alias: `ApprovedRule` is now [`ApprovalGrant`].
pub type ApprovedRule = ApprovalGrant;
