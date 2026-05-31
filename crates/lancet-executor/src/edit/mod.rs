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
pub use hash::content_hash;
