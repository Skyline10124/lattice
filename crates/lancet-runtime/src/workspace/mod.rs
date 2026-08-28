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
//! Tool Host `workspace-observe` and `workspace-mutate` imports plus Rewind
//! share this module. Network execution is owned by the sibling runtime host.

mod local;
pub mod rewind;
mod types;
mod visibility;

pub use local::{BoundedFileRead, LocalWorkspace};
pub(crate) use local::{ConditionalCreateError, ConditionalReplaceError};
pub use types::{
    CreatedFile, DirectoryEntry, FindRequest, GrepMatch, GrepRequest, InstalledFile,
    LocalWorkspaceError, PathFacts, PathIntent, PathKind, WorkspacePath, WorkspacePathError,
};
pub use visibility::{VisibilityEngine, VisibilityEvidence, join_rel};

/// Maximum bytes a single Atomic Local Mutation may write to one file (#120).
///
/// One definition for every live workspace write path. It is not a
/// profile/config axis: a Cell must not raise its own write budget. Rewind
/// reinstates bytes the workspace already held.
pub const MAX_WORKSPACE_WRITE_BYTES: usize = 1024 * 1024;

/// Canonical content hash for a byte slice.
///
/// `blake3:<64 hex>` is the one hash form the workspace speaks: `read_file`
/// observation returns it as `content_hash`, and `mutate.edit` verifies the
/// caller's `expected_hash` against it. Observation and mutation share this
/// exact implementation.
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
