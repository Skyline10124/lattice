// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Canonical JSON serialization — the single implementation behind the
//! deterministic context measure (#28 D12) and v2 attestation statement
//! bytes (D8).
//!
//! Object keys are sorted explicitly at every depth: `serde_json::Map`
//! iteration order is a build-time choice (the `preserve_order` feature),
//! never a canonicalization guarantee. Array order is data and is preserved.
//! No whitespace is emitted; scalars use serde_json's own formatting so
//! number rendering matches the rest of the wire surface.

use serde_json::Value;

/// Serialize `value` with byte-order–sorted object keys and no whitespace.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            // Collect and sort: never trust the map's own iteration order.
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        // Null/bool/number/string: serde_json's compact Display output.
        scalar => out.push_str(&scalar.to_string()),
    }
}

fn write_string(value: &str, out: &mut String) {
    // Escaping stays with serde_json; a hand-rolled escaper would drift from
    // the format used for every other string on this surface.
    out.push_str(&Value::String(value.to_owned()).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_affect_output() {
        // Same object, two insertion orders through the parser — the parser
        // is the only way to get a differently-ordered map under
        // `preserve_order`.
        let a: Value = serde_json::from_str(r#"{"b":1,"a":2,"c":3}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"c":3,"a":2,"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":2,"b":1,"c":3}"#);
    }

    #[test]
    fn nested_objects_are_sorted_at_every_depth() {
        let a: Value =
            serde_json::from_str(r#"{"z":{"y":{"x":1,"a":2},"b":3},"a":[{"q":1,"p":2}]}"#).unwrap();
        let b: Value =
            serde_json::from_str(r#"{"a":[{"p":2,"q":1}],"z":{"b":3,"y":{"a":2,"x":1}}}"#).unwrap();
        let canonical = canonical_json(&a);
        assert_eq!(canonical, canonical_json(&b));
        assert_eq!(
            canonical,
            r#"{"a":[{"p":2,"q":1}],"z":{"b":3,"y":{"a":2,"x":1}}}"#
        );
        // Stable under re-canonicalization: parsing the output reproduces it.
        let reparsed: Value = serde_json::from_str(&canonical).unwrap();
        assert_eq!(canonical_json(&reparsed), canonical);
    }

    #[test]
    fn array_order_is_preserved() {
        let value = json!({"items": [3, 1, 2, {"b": 1, "a": 2}, [9, 8]]});
        assert_eq!(
            canonical_json(&value),
            r#"{"items":[3,1,2,{"a":2,"b":1},[9,8]]}"#
        );
    }

    #[test]
    fn golden_canonical_form() {
        // Pins whitespace, key order, escaping, and scalar rendering — the
        // signing/measure input must not drift.
        let value: Value = serde_json::from_str(
            r#"{
                "s": "a\"b\né",
                "n": 1.5,
                "i": -3,
                "big": 9007199254740993,
                "t": true,
                "nil": null,
                "empty_obj": {},
                "empty_arr": [],
                "": "empty key"
            }"#,
        )
        .unwrap();
        assert_eq!(
            canonical_json(&value),
            r#"{"":"empty key","big":9007199254740993,"empty_arr":[],"empty_obj":{},"i":-3,"n":1.5,"nil":null,"s":"a\"b\né","t":true}"#
        );
    }

    #[test]
    fn output_carries_no_whitespace() {
        let value: Value =
            serde_json::from_str("{\n  \"a\" : [ 1 , 2 ] ,\n  \"b\" : { \"c\" : 3 }\n}").unwrap();
        let canonical = canonical_json(&value);
        assert!(
            !canonical.contains(' ') && !canonical.contains('\n'),
            "canonical form must be whitespace-free: {canonical}"
        );
    }

    #[test]
    fn scalar_roots_match_serde_json() {
        for value in [
            json!(null),
            json!(true),
            json!(0),
            json!(-1.25),
            json!("x"),
            json!([]),
            json!({}),
        ] {
            assert_eq!(
                canonical_json(&value),
                serde_json::to_string(&value).unwrap()
            );
        }
    }
}
