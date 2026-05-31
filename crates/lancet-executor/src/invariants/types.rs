//! Invariant card domain types (D-10, D-11, D-12).

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
/// `RustSymbol` is not matchable by path alone (conservatively included — degrades
/// to HumanReview per D-11 when the symbol cannot be resolved to a file).
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

/// An opaque cautionary note requiring HumanReview (D-12).
#[derive(Clone, Debug, Deserialize)]
pub struct OpaqueEntry {
    pub text: String,
}
