//! LATTICE policy types — approval grants, denial reports, risk classification,
//! and validation logic. This is the canonical home for policy types; crates that
//! previously defined their own copies should re-export from here.
//!
//! # Vocabulary (explicit approval model)
//!
//! | Type | Role |
//! |---|---|
//! | [`ApprovalKind`] | What is being allowed (command exact/prefix, path exact/recursive, tool, bundle) |
//! | [`ApprovalGrant`] | A validated approval rule — kind + accepted risk class |
//! | [`ApprovalProposalKind`] | What the model proposes to execute (command exact, path exact, tool) |
//! | [`ApprovalProposal`] | Full proposal: kind, risk, display text, approve command |
//! | [`PolicyDecision`] | Allow or Deny(DenialReport) for turn-engine integration |
//! | [`DenialReport`] | Structured denial with sentinel, kind, risk class, and suggested rule |
//! | [`DenialKind`] | Typed denial context (command, tool, path) |
//! | [`RiskClass`] | Read / Mutate / Destructive |
//! | [`PathOperation`] | Read / Write |

pub mod approval;
pub mod authority;
pub mod decision;
pub mod denial;
pub mod proposal;
pub mod risk;
pub mod validation;

// Public re-exports — preserve the exact flat API from before the module split.
pub use approval::{ApprovalGrant, ApprovalKind};
pub use authority::{
    command_matches_allowlist, command_matches_exact, is_path_prefix, path_matches_exact,
    path_matches_recursive, path_matches_static_allowlist, PolicyAuthority, ToolCatalog,
};
pub use decision::PolicyDecision;
pub use denial::{DenialKind, DenialReport};
pub use proposal::{
    approval_kind_from_approve_command, approval_proposal_for_denial,
    approval_proposal_marker_for_denial, quote_approval_arg, ApprovalProposal,
    ApprovalProposalKind,
};
pub use risk::{PathOperation, RiskClass};
pub use validation::{
    dangerous_command_pattern, validate_command_exact_rule, validate_command_prefix_rule,
    validate_command_rule, validate_path_rule, ValidationError, BLOCKED_TOP_LEVEL_PATHS,
    DANGEROUS_COMMAND_PATTERNS,
};
