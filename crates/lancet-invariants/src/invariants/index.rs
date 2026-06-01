//! Anchor-based invariant card index.

use crate::invariants::types::{CardAnchor, InvariantCard};
use globset::{Glob, GlobMatcher};

struct IndexedCard {
    card: InvariantCard,
    glob: Option<GlobMatcher>,
}

/// Pre-built index for efficient anchor-based card matching.
///
/// Constructed at session start. `Path` and `Glob` anchors are resolved
/// against target paths; `RustSymbol` anchors are conservatively included and
/// degrade to opaque/HumanReview when they cannot be resolved to a file.
pub struct InvariantCardIndex {
    entries: Vec<IndexedCard>,
}

impl std::fmt::Debug for InvariantCardIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvariantCardIndex")
            .field("card_count", &self.entries.len())
            .finish()
    }
}

impl InvariantCardIndex {
    /// Build the index from loaded cards. Globs containing `..` are rejected
    /// at compile time (defense-in-depth; the loader already rejects them).
    pub fn new(cards: Vec<InvariantCard>) -> Self {
        let mut entries = Vec::with_capacity(cards.len());
        for card in cards {
            let glob = match &card.anchor {
                CardAnchor::Glob(pattern) if pattern.contains("..") => {
                    tracing::warn!(
                        "skipping glob anchor with `..` for card {} (defense-in-depth)",
                        card.id
                    );
                    None
                }
                CardAnchor::Glob(pattern) => match Glob::new(pattern) {
                    Ok(glob) => Some(glob.compile_matcher()),
                    Err(err) => {
                        tracing::warn!(
                            "invalid glob pattern in card {}: {} — skipping glob match",
                            card.id,
                            err
                        );
                        None
                    }
                },
                _ => None,
            };
            entries.push(IndexedCard { card, glob });
        }
        Self { entries }
    }

    /// Return cards whose anchor matches `target_path`.
    ///
    /// - `Path` anchors match exact string equality.
    /// - `Glob` anchors match only that card's compiled glob pattern.
    /// - `RustSymbol` anchors always match as a conservative fallback.
    pub fn match_target(&self, target_path: &str) -> Vec<&InvariantCard> {
        self.entries
            .iter()
            .filter(|entry| match &entry.card.anchor {
                CardAnchor::Path(path) => path == target_path,
                CardAnchor::Glob(_) => entry
                    .glob
                    .as_ref()
                    .is_some_and(|glob| glob.is_match(target_path)),
                CardAnchor::RustSymbol(_) => true,
            })
            .map(|entry| &entry.card)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, anchor: CardAnchor) -> InvariantCard {
        InvariantCard {
            id: id.to_string(),
            title: "Test card".to_string(),
            anchor,
            forbidden: vec![],
            opaque: vec![],
        }
    }

    #[test]
    fn test_glob_anchor_matches() {
        let index = InvariantCardIndex::new(vec![card(
            "G1",
            CardAnchor::Glob("src/**/*.rs".to_string()),
        )]);
        let matches = index.match_target("src/main.rs");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "G1");
    }

    #[test]
    fn test_glob_anchor_does_not_cross_match_other_cards() {
        let index = InvariantCardIndex::new(vec![
            card("G1", CardAnchor::Glob("src/**/*.rs".to_string())),
            card("G2", CardAnchor::Glob("docs/**/*.md".to_string())),
        ]);
        let matches = index.match_target("src/main.rs");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "G1");
    }

    #[test]
    fn test_path_anchor_exact_match() {
        let index =
            InvariantCardIndex::new(vec![card("P1", CardAnchor::Path("src/lib.rs".to_string()))]);
        let matches = index.match_target("src/lib.rs");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "P1");

        let no_match = index.match_target("src/main.rs");
        assert!(no_match.is_empty());
    }

    #[test]
    fn test_no_match_returns_empty() {
        let index =
            InvariantCardIndex::new(vec![card("P1", CardAnchor::Path("src/lib.rs".to_string()))]);
        let matches = index.match_target("docs/README.md");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_rust_symbol_always_matches() {
        let index = InvariantCardIndex::new(vec![card(
            "RS1",
            CardAnchor::RustSymbol("my_crate::my_fn".to_string()),
        )]);
        let matches = index.match_target("any/random/path.rs");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "RS1");
    }

    #[test]
    fn test_empty_index() {
        let index = InvariantCardIndex::new(vec![]);
        assert!(index.is_empty());
        assert!(index.match_target("anything").is_empty());
    }

    #[test]
    fn test_dotdot_glob_rejected_at_index_time() {
        let index = InvariantCardIndex::new(vec![card(
            "EVIL",
            CardAnchor::Glob("../escape/**".to_string()),
        )]);
        assert!(index.match_target("src/lib.rs").is_empty());
    }
}
