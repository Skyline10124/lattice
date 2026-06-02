use super::diagnostics::parse_error;
use super::types::PatchDiagnostic;

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
}
