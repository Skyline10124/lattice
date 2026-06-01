//! Invariant card domain types.

use serde::Deserialize;

/// A structural invariant card loaded from `.lancet/invariants/*.toml`.
///
/// Cards declaratively describe structural constraints (machine-decidable)
/// and opaque cautionary notes (HumanReview only) keyed by file-system anchor.
#[derive(Clone, Debug, Deserialize)]
pub struct InvariantCard {
    pub id: String,
    pub title: String,
    pub anchor: CardAnchor,
    #[serde(default)]
    pub forbidden: Vec<ForbiddenEntry>,
    #[serde(default)]
    pub opaque: Vec<OpaqueEntry>,
}

/// Anchor for matching a card against a target path.
///
/// `Path` matches exact strings; `Glob` uses globset multi-pattern matching;
/// `RustSymbol` is not matchable by path alone, so it is conservatively included
/// and degrades to HumanReview when the symbol cannot be resolved to a file.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum CardAnchor {
    Path(String),
    Glob(String),
    RustSymbol(String),
}

/// A machine-decidable forbidden structural constraint.
#[derive(Clone, Debug, Deserialize)]
pub struct ForbiddenEntry {
    pub category: String,
    pub reason: String,
}

/// An opaque cautionary note requiring HumanReview.
#[derive(Clone, Debug, Deserialize)]
pub struct OpaqueEntry {
    pub text: String,
}
