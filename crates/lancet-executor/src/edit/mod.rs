//! Structured patch parsing and result types for editing tools.
//!
//! This module owns the typed edit IR, filesystem apply engine, and
//! recovery-oriented diagnostics used by the `patch` tool.

pub mod apply;
pub mod diagnostics;
pub mod hash;
pub mod parser;
pub mod types;

pub use apply::apply_patch;
pub use diagnostics::parse_error;
pub use hash::{content_hash, parse_old_hash};
pub use parser::{parse_patch_document, PatchParseError};
pub use types::{
    ApplyPlan, AttemptedOperation, ChangedFile, FileOperationKind, PatchDiagnostic, PatchDocument,
    PatchErrorCode, PatchHunk, PatchHunkLine, PatchOperation, PatchResultEnvelope,
    PlannedFileChange,
};
