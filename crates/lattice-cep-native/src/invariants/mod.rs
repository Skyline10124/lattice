//! Invariant card system for CEP.
//!
//! Loads `.cep/invariants/*.toml` cards, indexes by anchor, and provides
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
    ".cep/invariants/"
}

/// Legacy default invariant card directory used before the CEP rename.
pub fn legacy_invariants_dir() -> &'static str {
    ".lancet/invariants/"
}

/// Resolve the configured invariant directory against `project_root`.
///
/// When no directory is explicitly configured, prefer the CEP default but fall
/// back to the pre-CEP `.lancet/invariants/` location if the CEP directory is
/// unavailable and legacy cards are still present. This preserves upgraded project
/// policies instead of silently loading an empty invariant index.
pub fn resolve_invariants_dir(project_root: &Path, configured: Option<&str>) -> PathBuf {
    if let Some(dir) = configured {
        return resolve_invariants_path(project_root, dir);
    }

    let default_path = resolve_invariants_path(project_root, default_invariants_dir());
    if default_path.is_dir() {
        return default_path;
    }

    let legacy_path = resolve_invariants_path(project_root, legacy_invariants_dir());
    if legacy_path.is_dir() {
        tracing::warn!(
            "using legacy invariant card directory {}; migrate cards to {}",
            legacy_path.display(),
            default_path.display()
        );
        return legacy_path;
    }

    default_path
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
    fn default_resolution_falls_back_to_legacy_dir_when_cep_dir_is_absent() {
        let root = tempfile::tempdir().unwrap();
        let legacy_dir = root.path().join(legacy_invariants_dir());
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(
            legacy_dir.join("no-functions.toml"),
            r#"
id = "INV-LEGACY"
title = "Legacy invariant"
anchor = { type = "path", value = "src/lib.rs" }

[[forbidden]]
category = "function_def"
reason = "exercise legacy fallback"
"#,
        )
        .unwrap();

        let resolved = resolve_invariants_dir(root.path(), None);
        assert_eq!(resolved, legacy_dir);

        let index = load_invariant_index_from_dir(&resolved);
        let matches = index.match_target("src/lib.rs");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "INV-LEGACY");
    }

    #[test]
    fn default_resolution_prefers_cep_dir_when_both_dirs_exist() {
        let root = tempfile::tempdir().unwrap();
        let cep_dir = root.path().join(default_invariants_dir());
        let legacy_dir = root.path().join(legacy_invariants_dir());
        std::fs::create_dir_all(&cep_dir).unwrap();
        std::fs::create_dir_all(&legacy_dir).unwrap();

        let resolved = resolve_invariants_dir(root.path(), None);
        assert_eq!(resolved, cep_dir);
    }

    #[test]
    fn configured_resolution_does_not_fall_back_to_legacy_dir() {
        let root = tempfile::tempdir().unwrap();
        let legacy_dir = root.path().join(legacy_invariants_dir());
        std::fs::create_dir_all(&legacy_dir).unwrap();

        let resolved = resolve_invariants_dir(root.path(), Some("custom/invariants"));
        assert_eq!(resolved, root.path().join("custom/invariants"));
    }
}
