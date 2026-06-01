//! Opaque invariant card handling.

use crate::invariants::InvariantCard;
use lancet_protocol::error::{CepErrorData, GuardErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};

/// Collect opaque review notes from matched cards for envelope metadata.
pub fn opaque_notes(cards: &[&InvariantCard]) -> Vec<serde_json::Value> {
    cards
        .iter()
        .flat_map(|card| {
            card.opaque.iter().map(|entry| {
                serde_json::json!({
                    "card_id": card.id,
                    "text": entry.text,
                })
            })
        })
        .collect()
}

/// Build a human-readable message when matched cards require review.
pub fn opaque_human_review_message(cards: &[&InvariantCard]) -> Option<String> {
    let notes: Vec<String> = cards
        .iter()
        .flat_map(|card| {
            card.opaque
                .iter()
                .map(|entry| format!("[{}] {}", card.id, entry.text))
        })
        .collect();
    if notes.is_empty() {
        None
    } else {
        Some(notes.join("; "))
    }
}

/// Error returned when opaque invariant cards require human review before mutation.
pub fn opaque_human_review_error(method: &str, reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        format!("invariant card requires human review: {reason}"),
        CepErrorData::Guard(GuardErrorData {
            method: method.into(),
            reason: format!("HumanReview: {reason}"),
        }),
        Retriability::Unknown,
    )
}
