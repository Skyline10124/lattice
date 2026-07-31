// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete Local Workspace module (ADR 0003 decision 3).
//!
//! This is the single host-side filesystem source of truth for LANCET
//! observation and mutation. The module is an internal deep module: it hides
//! secure path resolution, symlink handling, bounded reads, content hashing,
//! atomic replacement, and filesystem error normalization behind one concrete
//! type. It is not a public pluggable execution-backend interface.
//!
//! GitHub #30 moved the module down from `lancet-native` into this crate: the
//! Tool Host's `workspace-observe` imports (`host_workspace.rs`) execute
//! observation for the `observe.workspace` Component, and a host import cannot
//! reach up a layer. Mutation and rewind still run from `lancet-native`
//! handlers, which now consume this module through the crate's public surface
//! instead of owning it — one concrete host module, as the workspace contract
//! requires, rather than two copies of secure path resolution. #31/#32 move the
//! remaining mutation callers behind Component Tools.

mod local;
pub mod rewind;
mod types;
mod visibility;

pub use local::LocalWorkspace;
pub use types::{
    CreatedFile, DirectoryEntry, FindRequest, GrepMatch, GrepRequest, InstalledFile,
    LocalWorkspaceError, PathFacts, PathIntent, PathKind, WorkspacePath, WorkspacePathError,
};
pub use visibility::{VisibilityEngine, VisibilityEvidence, join_rel};

/// Canonical content hash for a byte slice.
///
/// `blake3:<64 hex>` is the one hash form the workspace speaks: `read_file`
/// observation returns it as `content_hash`, and `mutate.edit` verifies the
/// caller's `expected_hash` against it. It lives beside the workspace rather
/// than in the edit engine so the observation host imports can hash a file
/// without `lancet-runtime` reaching up into `lancet-native`'s patch parser
/// (which owns the *other* hash forms — hashline anchors — and stays there
/// until #32).
pub fn content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(test)]
mod tests {
    use super::content_hash;

    /// The digest form is a wire contract: `observe.workspace` hands this exact
    /// string to the model and `mutate.edit` compares it byte-for-byte, so the
    /// prefix and hex width are pinned, not incidental.
    #[test]
    fn content_hash_is_prefixed_lowercase_blake3_hex() {
        let hash = content_hash(b"fixture");
        let hex = hash.strip_prefix("blake3:").expect("blake3: prefix");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
        assert_eq!(hash, content_hash(b"fixture"));
        assert_ne!(hash, content_hash(b"fixture "));
    }
}
