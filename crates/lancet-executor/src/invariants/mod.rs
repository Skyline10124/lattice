//! Invariant card system for CEP.
//!
//! Loads `.lancet/invariants/*.toml` cards, indexes by anchor, and provides
//! target-path-based queries that feed structural constraints into the
//! `submit_intent` handler (Plan 03).

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

/// Default invariant card directory relative to the project root (D-09).
pub fn default_invariants_dir() -> &'static str {
    ".lancet/invariants/"
}

/// Resolve the configured invariant directory against `project_root`.
pub fn resolve_invariants_dir(project_root: &Path, configured: Option<&str>) -> PathBuf {
    let dir = configured.unwrap_or(default_invariants_dir());
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
