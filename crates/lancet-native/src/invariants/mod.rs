//! Compatibility re-exports for CEP invariant cards.

pub mod index {
    pub use lancet_invariants::invariants::index::InvariantCardIndex;
}
pub mod loader {
    pub use lancet_invariants::invariants::loader::{
        load_cards_from_dir, CardParseError, InvariantLoadError,
    };
}
pub mod review {
    pub use lancet_invariants::invariants::review::{
        opaque_human_review_error, opaque_human_review_message, opaque_notes,
    };
}
pub mod types {
    pub use lancet_invariants::invariants::types::{
        CardAnchor, ForbiddenEntry, InvariantCard, OpaqueEntry,
    };
}

pub use lancet_invariants::{
    categories_in_byte_range, check_l2, default_invariants_dir, l2_status_for_method,
    load_cards_from_dir, load_invariant_index_from_dir, node_kind_to_category,
    opaque_human_review_error, opaque_human_review_message, opaque_notes, resolve_invariants_dir,
    AstCategory, CardAnchor, ForbiddenEntry, InvariantCard, InvariantCardIndex, L2CheckResult,
    L2Violation, OpaqueEntry,
};
