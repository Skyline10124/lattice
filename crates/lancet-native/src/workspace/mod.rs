// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete Local Workspace module (ADR 0003 decision 3).
//!
//! This is the single host-side filesystem source of truth for LANCET
//! observation and mutation during the Direct-Local migration. The module is
//! an internal deep module: it hides secure path resolution, symlink
//! handling, bounded reads, content hashing, atomic replacement, and
//! filesystem error normalization behind one concrete type. It is not a
//! public pluggable execution-backend interface.
//!
//! Cross-cutting host responsibilities (filesystem reads/writes, network
//! mutation, audit, effect-contract execution) currently live in
//! `lancet-native`; ADR 0004 moves them into `lancet-runtime`.

mod local;
mod types;
mod visibility;

pub(crate) use local::LocalWorkspace;
pub(crate) use types::{CreatedFile, InstalledFile, LocalWorkspaceError};
pub(crate) use visibility::{VisibilityEngine, join_rel};
pub use types::{
    DirectoryEntry, FindRequest, GrepRequest, PathFacts, PathIntent, PathKind, WorkspacePath,
};
