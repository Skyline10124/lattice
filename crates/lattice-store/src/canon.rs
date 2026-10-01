// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Canonical JSON bytes for kernel-owned persisted records.
//!
//! One canonical form: object keys sorted, no insignificant whitespace,
//! arrays in order. The kernel is Layer 0 and cannot reuse
//! `lancet-protocol::canonical_json`; the rule is small enough to state here
//! and pin with tests. Meta records are the only kernel consumer.

use serde::Serialize;

use crate::StoreError;

/// Serialize `value` to canonical JSON bytes (sorted keys, compact).
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    let value = serde_json::to_value(value)
        .map_err(|e| StoreError::Git(format!("canonicalize to JSON value: {e}")))?;
    let mut out = Vec::new();
    write_value(&value, &mut out);
    Ok(out)
}

fn write_value(value: &serde_json::Value, out: &mut Vec<u8>) {
    match value {
        serde_json::Value::Object(map) => {
            out.push(b'{');
            // serde_json::Map preserves insertion order; sort for canonical
            // form. (The `preserve_order` feature is on via transitive deps,
            // so do not rely on BTreeMap iteration.)
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
            for (i, (key, val)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                // JSON string escaping of a map key is itself canonical under
                // serde_json (no optional whitespace, deterministic escapes).
                let key_bytes = serde_json::to_vec(key).expect("string serialization cannot fail");
                out.extend_from_slice(&key_bytes);
                out.push(b':');
                write_value(val, out);
            }
            out.push(b'}');
        }
        serde_json::Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(item, out);
            }
            out.push(b']');
        }
        scalar => {
            out.extend_from_slice(
                &serde_json::to_vec(scalar).expect("scalar serialization cannot fail"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_keys_sorted_and_compact() {
        let value = serde_json::json!({"b": 1, "a": {"d": true, "c": [2, 1]}});
        let bytes = canonical_json_bytes(&value).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"a":{"c":[2,1],"d":true},"b":1}"#
        );
    }
}
