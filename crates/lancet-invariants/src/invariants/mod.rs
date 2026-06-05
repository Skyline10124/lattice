// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Invariant card system for LANCET.
//!
//! Loads `.lancet/invariants/*.toml` cards, indexes by anchor, and provides
//! target-path-based queries that feed structural constraints into the
//! `submit_intent` handler.

pub mod index;
pub mod loader;
pub mod review;
pub mod types;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use index::InvariantCardIndex;
pub use loader::load_cards_from_dir;
pub use review::{opaque_human_review_error, opaque_human_review_message, opaque_notes};
pub use types::{CardAnchor, ForbiddenEntry, InvariantCard, OpaqueEntry};

/// Default invariant card directory relative to the project root.
pub fn default_invariants_dir() -> &'static str {
    ".lancet/invariants/"
}

/// Resolve the configured invariant directory against `project_root`.
pub fn resolve_invariants_dir(project_root: &Path, configured: Option<&str>) -> PathBuf {
    if let Some(dir) = configured {
        return resolve_invariants_path(project_root, dir);
    }

    resolve_invariants_path(project_root, default_invariants_dir())
}

fn resolve_invariants_path(project_root: &Path, dir: &str) -> PathBuf {
    let path = Path::new(dir);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

/// Load cards from disk and build an index. Logs and returns an empty index on failure.
pub fn load_invariant_index_from_dir(dir: &Path) -> Arc<InvariantCardIndex> {
    match load_cards_from_dir(dir) {
        Ok(cards) => Arc::new(InvariantCardIndex::new(cards)),
        Err(err) => {
            tracing::warn!("invariant card load failed for {}: {err}", dir.display());
            Arc::new(InvariantCardIndex::new(vec![]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_resolution_uses_lancet_dir() {
        let root = tempfile::tempdir().unwrap();
        let lancet_dir = root.path().join(default_invariants_dir());
        std::fs::create_dir_all(&lancet_dir).unwrap();
        std::fs::write(
            lancet_dir.join("no-functions.toml"),
            r#"
id = "INV-LANCET"
title = "LANCET invariant"
anchor = { type = "path", value = "src/lib.rs" }

[[forbidden]]
category = "function_def"
reason = "exercise default resolution"
"#,
        )
        .unwrap();

        let resolved = resolve_invariants_dir(root.path(), None);
        assert_eq!(resolved, lancet_dir);

        let index = load_invariant_index_from_dir(&resolved);
        let matches = index.match_target("src/lib.rs");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "INV-LANCET");
    }

    #[test]
    fn configured_resolution_overrides_default_dir() {
        let root = tempfile::tempdir().unwrap();
        let lancet_dir = root.path().join(default_invariants_dir());
        std::fs::create_dir_all(&lancet_dir).unwrap();

        let resolved = resolve_invariants_dir(root.path(), Some("custom/invariants"));
        assert_eq!(resolved, root.path().join("custom/invariants"));
    }
}
