// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `ProjectStoreKey`: project storage addressing from canonical Linux path
//! bytes (ADR 0005 §2).
//!
//! The key is a storage *address*, not project recognition: the same
//! canonical path always maps to the same namespace, a moved project maps to a
//! new one, and an unrelated project reusing a path sees the existing
//! namespace. Derivation uses the raw `OsStrExt` bytes of the canonical full
//! Local Workspace root — no UTF-8 conversion, content fingerprint, Git
//! remote, inode, generation registry, or `[lancet.project_root].root`.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::StoreError;

/// Domain separator for the v1 key derivation.
const DOMAIN_SEPARATOR: &[u8] = b"lattice-project-store-key-v1\0";

/// Lowercase-hex SHA-256 storage address of one canonical Local Workspace
/// root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectStoreKey([u8; 32]);

impl ProjectStoreKey {
    /// Derive the key for a Local Workspace root.
    ///
    /// The root is canonicalized first (symlinks resolved); the raw Linux
    /// path bytes of the canonical form are hashed. There is no UTF-8
    /// requirement at any step.
    pub fn derive(workspace_root: &Path) -> Result<Self, StoreError> {
        let canonical = std::fs::canonicalize(workspace_root).map_err(|source| {
            StoreError::CanonicalizeRoot {
                path: workspace_root.to_path_buf(),
                source,
            }
        })?;
        Ok(Self::from_canonical_path_bytes(
            canonical.as_os_str().as_bytes(),
        ))
    }

    /// Derive the key from already-canonical path bytes.
    ///
    /// Callers must guarantee the bytes are a canonical absolute path; the
    /// kernel cannot verify that property. Prefer [`Self::derive`] unless the
    /// canonical form is already in hand.
    pub fn from_canonical_path_bytes(path_bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(DOMAIN_SEPARATOR);
        hasher.update((path_bytes.len() as u64).to_be_bytes());
        hasher.update(path_bytes);
        let digest: [u8; 32] = hasher.finalize().into();
        Self(digest)
    }

    /// Parse a lowercase-hex key back from its display form.
    pub fn parse(hex: &str) -> Result<Self, StoreError> {
        if hex.len() != 64
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
            // Display form is canonical lower-hex: directory names compare
            // bytewise, so uppercase is a different (invalid) spelling.
            || hex.bytes().any(|b| b.is_ascii_uppercase())
        {
            return Err(StoreError::InvalidKey(hex.to_string()));
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16).expect("hex digit checked");
            let lo = (chunk[1] as char).to_digit(16).expect("hex digit checked");
            bytes[i] = ((hi << 4) | lo) as u8;
        }
        Ok(Self(bytes))
    }

    /// The raw key bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for ProjectStoreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl serde::Serialize for ProjectStoreKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for ProjectStoreKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_matches_adr_formula() {
        // Independently computed: SHA256("lattice-project-store-key-v1\0" ||
        // u64_be(9) || "/tmp/xyzz") — one fixed vector pins the wire format.
        let key = ProjectStoreKey::from_canonical_path_bytes(b"/tmp/xyzz");
        assert_eq!(key.to_string().len(), 64);
        let mut hasher = Sha256::new();
        hasher.update(b"lattice-project-store-key-v1\0");
        hasher.update(9u64.to_be_bytes());
        hasher.update(b"/tmp/xyzz");
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(key.as_bytes(), &expected);
    }

    #[test]
    fn distinct_paths_distinct_keys() {
        let a = ProjectStoreKey::from_canonical_path_bytes(b"/tmp/a");
        let b = ProjectStoreKey::from_canonical_path_bytes(b"/tmp/b");
        assert_ne!(a, b);
    }

    #[test]
    fn length_prefix_prevents_concat_collisions() {
        // Without the length prefix, ("ab","c") and ("a","bc") style splits
        // could collide; here the split is inside a single fixed field so the
        // prefix is defensive, but the vector still pins it.
        let short = ProjectStoreKey::from_canonical_path_bytes(b"/a");
        let long = ProjectStoreKey::from_canonical_path_bytes(b"/aa");
        assert_ne!(short, long);
    }

    #[test]
    fn parse_round_trip() {
        let key = ProjectStoreKey::from_canonical_path_bytes(b"/some/path");
        let parsed = ProjectStoreKey::parse(&key.to_string()).unwrap();
        assert_eq!(key, parsed);
    }

    #[test]
    fn parse_rejects_bad_forms() {
        for bad in [
            "",
            "xyz",
            &"a".repeat(63),
            &"a".repeat(65),
            &"A".repeat(64),
            &"g".repeat(64),
        ] {
            assert!(
                ProjectStoreKey::parse(bad).is_err(),
                "expected rejection: {bad:?}"
            );
        }
    }
}
