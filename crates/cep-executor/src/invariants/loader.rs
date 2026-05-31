//! TOML card loading from `.cep/invariants/` directories (D-09, D-13).

use crate::invariants::types::{CardAnchor, InvariantCard};
use std::path::{Path, PathBuf};

/// Errors that can occur during invariant card loading.
#[derive(Debug, thiserror::Error)]
pub enum InvariantLoadError {
    #[error("I/O error reading card directory at {}: {source}", path.display())]
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },
    /// One or more cards failed to parse. Individual parse failures are
    /// collected so the loader can continue past bad cards.
    #[error("{} card(s) failed to parse", .0.len())]
    ParseErrors(Vec<CardParseError>),
}

/// A single card parse failure with file path and message.
#[derive(Debug)]
pub struct CardParseError {
    pub path: PathBuf,
    pub message: String,
}

/// Load invariant cards from a directory of `*.toml` files.
///
/// Returns `Ok(vec![])` if `dir` does not exist (absent directory is not an
/// error per D-09 / PROF-05). Reads files non-recursively. Caps loaded cards
/// at 1000 (T-25-06). Rejects glob anchors containing `..` segments
/// (T-25-05).
pub fn load_cards_from_dir(dir: &Path) -> Result<Vec<InvariantCard>, InvariantLoadError> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    if !dir.is_dir() {
        return Ok(vec![]);
    }

    let mut cards = Vec::new();
    let mut parse_errors = Vec::new();

    let entries = std::fs::read_dir(dir).map_err(|e| InvariantLoadError::IoError {
        path: dir.to_path_buf(),
        source: e,
    })?;

    for entry in entries {
        if cards.len() >= 1000 {
            tracing::warn!("invariant card count exceeds 1000 — truncating (T-25-06)");
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }

        // Validate before deserializing: reject glob anchors with `..` (T-25-05).
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("skipping unreadable card {:?}: {e}", path);
                parse_errors.push(CardParseError {
                    path: path.clone(),
                    message: format!("I/O error: {e}"),
                });
                continue;
            }
        };

        match toml::from_str::<InvariantCard>(&content) {
            Ok(card) => {
                // Post-deserialize validation: check anchor value specifically.
                if let CardAnchor::Glob(ref pattern) = card.anchor {
                    if pattern.contains("..") {
                        tracing::warn!(
                            "rejecting card {:?}: glob anchor contains `..` (T-25-05)",
                            path
                        );
                        parse_errors.push(CardParseError {
                            path: path.clone(),
                            message: "glob anchor contains `..` (T-25-05)".to_string(),
                        });
                        continue;
                    }
                }
                cards.push(card);
            }
            Err(e) => {
                tracing::warn!("skipping invalid card {:?}: {e}", path);
                parse_errors.push(CardParseError {
                    path: path.clone(),
                    message: format!("{e}"),
                });
            }
        }
    }

    if !parse_errors.is_empty() && cards.is_empty() {
        return Err(InvariantLoadError::ParseErrors(parse_errors));
    }
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_card(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
    }

    #[test]
    fn test_load_cards_from_nonexistent_dir_returns_empty() {
        let dir = Path::new("/tmp/nonexistent_invariant_dir_xyz");
        let result = load_cards_from_dir(dir).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_valid_card_toml() {
        let dir = tempfile::tempdir().unwrap();
        write_card(
            dir.path(),
            "card.toml",
            r#"
id = "INV-001"
title = "No unsafe in core"
anchor = { type = "glob", value = "crates/cep-executor/src/**/*.rs" }

[[forbidden]]
category = "unsafe"
reason = "All unsafe blocks must be audited by security review"

[[opaque]]
text = "Review error handling in this module for edge cases"
"#,
        );
        let cards = load_cards_from_dir(dir.path()).unwrap();
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card.id, "INV-001");
        assert_eq!(card.title, "No unsafe in core");
        assert_eq!(card.forbidden.len(), 1);
        assert_eq!(card.opaque.len(), 1);
    }

    #[test]
    fn test_load_invalid_toml_continues() {
        let dir = tempfile::tempdir().unwrap();
        write_card(dir.path(), "bad.toml", "this is not valid toml {{{");
        write_card(
            dir.path(),
            "good.toml",
            r#"
id = "OK-001"
title = "Valid card"
anchor = { type = "path", value = "src/lib.rs" }
"#,
        );
        let cards = load_cards_from_dir(dir.path()).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "OK-001");
    }

    #[test]
    fn test_glob_anchor_with_dotdot_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_card(
            dir.path(),
            "escape.toml",
            r#"
id = "ESC-001"
title = "Escape attempt"
anchor = { type = "glob", value = "../escape/**" }
"#,
        );
        // Glob anchor value contains `..` and is rejected after deserialization.
        let result = load_cards_from_dir(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_reason_with_dotdot_in_text_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        write_card(
            dir.path(),
            "note.toml",
            r#"
id = "NOTE-001"
title = "Parent dir note"
anchor = { type = "path", value = "src/lib.rs" }

[[opaque]]
text = "Avoid parent dir (..) traversal patterns in this module"
"#,
        );
        let cards = load_cards_from_dir(dir.path()).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "NOTE-001");
    }

    #[test]
    fn test_non_toml_files_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_card(dir.path(), "notes.md", "# Just notes");
        write_card(
            dir.path(),
            "card.toml",
            r#"
id = "INV-SKIP"
title = "Only TOML loaded"
anchor = { type = "path", value = "src/lib.rs" }
"#,
        );
        let cards = load_cards_from_dir(dir.path()).unwrap();
        assert_eq!(cards.len(), 1);
    }
}
