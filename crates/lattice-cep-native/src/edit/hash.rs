use super::diagnostics::parse_error;
use super::types::PatchDiagnostic;

const HASH_PREFIX: &str = "blake3:";
const BLAKE3_HEX_LEN: usize = 64;

pub fn content_hash(bytes: &[u8]) -> String {
    format!("{HASH_PREFIX}{}", blake3::hash(bytes).to_hex())
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
}
