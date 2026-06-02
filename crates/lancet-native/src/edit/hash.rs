use super::diagnostics::parse_error;
use super::types::{HashLine, PatchDiagnostic};

const HASH_PREFIX: &str = "blake3:";
const BLAKE3_HEX_LEN: usize = 64;
const LINE_HASH_HEX_LEN: usize = 4;
const LINE_HASH_DOMAIN: &[u8] = b"lattice.hashline.v2\0";

pub fn content_hash(bytes: &[u8]) -> String {
    format!("{HASH_PREFIX}{}", blake3::hash(bytes).to_hex())
}

pub fn line_hash(line_number: usize, line: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(LINE_HASH_DOMAIN);
    hasher.update(&(line_number as u64).to_be_bytes());
    hasher.update(b"\0");
    hasher.update(line.as_bytes());
    let digest = hasher.finalize();
    let hex = digest.to_hex();
    format!("{HASH_PREFIX}{}", &hex[..LINE_HASH_HEX_LEN])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HashlineBaselineError {
    FileMissing,
    CurrentContentNotUtf8 { error: String },
    AnchorMismatch { hash: String, text: String },
}

impl HashlineBaselineError {
    #[allow(dead_code)]
    pub(crate) fn requires_replan(&self) -> bool {
        true
    }
}

#[allow(dead_code)]
pub(crate) fn verify_hashline_baseline(
    anchors: &[HashLine],
    current_content: Option<&[u8]>,
) -> Result<(), HashlineBaselineError> {
    let Some(current_content) = current_content else {
        return Err(HashlineBaselineError::FileMissing);
    };
    let current = std::str::from_utf8(current_content).map_err(|error| {
        HashlineBaselineError::CurrentContentNotUtf8 {
            error: error.to_string(),
        }
    })?;
    let current_lines = split_hashline_lines(current);

    for anchor in anchors {
        let matched = current_lines
            .iter()
            .enumerate()
            .any(|(index, line)| *line == anchor.text && line_hash(index + 1, line) == anchor.hash);
        if !matched {
            return Err(HashlineBaselineError::AnchorMismatch {
                hash: anchor.hash.clone(),
                text: anchor.text.clone(),
            });
        }
    }

    Ok(())
}

fn split_hashline_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

pub fn parse_old_hash(value: &str) -> Result<String, PatchDiagnostic> {
    if !value.starts_with(HASH_PREFIX) {
        return Err(parse_error(
            "old hash must use canonical blake3:<64 hex> form",
        ));
    }

    let hex = &value[HASH_PREFIX.len()..];
    if hex.len() != BLAKE3_HEX_LEN || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(parse_error(
            "old hash must use canonical blake3:<64 hex> form",
        ));
    }

    Ok(format!("{HASH_PREFIX}{}", hex.to_ascii_lowercase()))
}

pub fn parse_line_hash(value: &str) -> Result<String, PatchDiagnostic> {
    if !value.starts_with(HASH_PREFIX) {
        return Err(parse_error(
            "line hash must use canonical blake3:<4 hex> form",
        ));
    }

    let hex = &value[HASH_PREFIX.len()..];
    if hex.len() != LINE_HASH_HEX_LEN || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(parse_error(
            "line hash must use canonical blake3:<4 hex> form",
        ));
    }

    Ok(format!("{HASH_PREFIX}{}", hex.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(line_number: usize, text: &str) -> HashLine {
        HashLine {
            hash: line_hash(line_number, text),
            text: text.to_string(),
        }
    }

    #[test]
    fn content_hash_generates_blake3_prefix() {
        let hash = content_hash(b"lattice");

        assert!(hash.starts_with("blake3:"));
        assert_eq!(hash.len(), "blake3:".len() + 64);
    }

    #[test]
    fn parse_old_hash_rejects_raw_hex_without_blake3_prefix() {
        let raw = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        let err = parse_old_hash(raw).unwrap_err();

        assert_eq!(err.code.as_str(), "parse_error");
        assert!(err.message.contains("blake3:<64 hex>"));
    }

    #[test]
    fn line_hash_uses_line_number_and_short_prefix() {
        let first = line_hash(1, "same");
        let second = line_hash(2, "same");

        assert!(first.starts_with("blake3:"));
        assert_eq!(first.len(), "blake3:".len() + 4);
        assert_ne!(first, second);
    }

    #[test]
    fn parse_line_hash_rejects_full_hash() {
        let full = content_hash(b"line");

        let err = parse_line_hash(&full).unwrap_err();

        assert_eq!(err.code.as_str(), "parse_error");
        assert!(err.message.contains("blake3:<4 hex>"));
    }

    #[test]
    fn verify_hashline_baseline_accepts_unchanged_current_content() {
        let current = b"alpha\nbeta\nomega\n";
        let anchors = vec![anchor(1, "alpha"), anchor(2, "beta"), anchor(3, "omega")];

        let result = verify_hashline_baseline(&anchors, Some(current));

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn verify_hashline_baseline_rejects_hash_mismatch_as_replan_required() {
        let current = b"inserted\nalpha\nbeta\nomega\n";
        let anchor = anchor(2, "beta");

        let err = verify_hashline_baseline(std::slice::from_ref(&anchor), Some(current))
            .expect_err("shifted anchor line must invalidate the baseline");

        assert_eq!(
            err,
            HashlineBaselineError::AnchorMismatch {
                hash: anchor.hash,
                text: anchor.text,
            }
        );
        assert!(err.requires_replan());
    }

    #[test]
    fn verify_hashline_baseline_rejects_missing_file_or_anchor_as_replan_required() {
        let anchor = anchor(2, "beta");

        let missing_file = verify_hashline_baseline(std::slice::from_ref(&anchor), None)
            .expect_err("missing files must invalidate the baseline");

        assert_eq!(missing_file, HashlineBaselineError::FileMissing);
        assert!(missing_file.requires_replan());

        let deleted_line =
            verify_hashline_baseline(std::slice::from_ref(&anchor), Some(b"alpha\nomega\n"))
                .expect_err("deleted anchor lines must invalidate the baseline");

        assert_eq!(
            deleted_line,
            HashlineBaselineError::AnchorMismatch {
                hash: anchor.hash,
                text: anchor.text,
            }
        );
        assert!(deleted_line.requires_replan());
    }
}
