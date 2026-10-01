// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Canonical JSON serialization — the single implementation behind the
//! deterministic context measures and versioned Cell-evidence construction.
//!
//! Object keys are sorted explicitly at every depth: `serde_json::Map`
//! iteration order is a build-time choice (the `preserve_order` feature),
//! never a canonicalization guarantee. Array order is data and is preserved.
//! No whitespace is emitted; scalars use serde_json's own formatting so
//! number rendering matches the rest of the wire surface.
//!
//! The workspace builds serde_json without `preserve_order`, so `Map` is a
//! `BTreeMap` and no `Value` can present unsorted entries: the sort is
//! forward defense against a future feature unification and is **not
//! observable through `canonical_json` today**. That is why [`write_object`]
//! takes an entry sequence rather than a `Map` — it is the one seam where
//! the sort can be driven, and `object_entries_are_sorted_by_the_writer`
//! is what keeps it alive.

use serde_json::Value;

/// Serialize `value` with byte-order–sorted object keys and no whitespace.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => write_object(
            map.iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect(),
            out,
        ),
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

/// Write one object from its entries, sorted by key. Takes a sequence rather
/// than a `Map` so the sort is exercisable without a `preserve_order` build.
fn write_object(mut entries: Vec<(&str, &Value)>, out: &mut String) {
    entries.sort_unstable_by_key(|(key, _)| *key);
    out.push('{');
    for (index, (key, value)) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_string(key, out);
        out.push(':');
        write_value(value, out);
    }
    out.push('}');
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

    /// The only non-tautological guard on the sort. `serde_json::Map` is a
    /// `BTreeMap` in this workspace, so every `Value`-level order assertion
    /// below would pass with the sort deleted; this one drives the writer's
    /// entry sequence directly, which is what a `preserve_order` build would
    /// hand it.
    #[test]
    fn object_entries_are_sorted_by_the_writer() {
        let (one, two, three) = (json!(1), json!(2), json!({"z": 0, "a": 1}));
        let mut out = String::new();
        write_object(vec![("b", &one), ("", &three), ("a", &two)], &mut out);
        assert_eq!(out, r#"{"":{"a":1,"z":0},"a":2,"b":1}"#);
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
    fn canonical_json_preserves_value_representation() {
        let golden_name = "golden_canonical_form";
        let golden: Value = serde_json::from_str(
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
        .unwrap_or_else(|err| panic!("{golden_name}: {err}"));
        for (name, input, expected) in [
            (
                "array_order_is_preserved",
                json!({"items": [3, 1, 2, {"b": 1, "a": 2}, [9, 8]]}),
                Some(r#"{"items":[3,1,2,{"a":2,"b":1},[9,8]]}"#),
            ),
            // Pins whitespace, key order, escaping, and scalar rendering — the
            // signing/measure input must not drift.
            (
                golden_name,
                golden,
                Some(
                    r#"{"":"empty key","big":9007199254740993,"empty_arr":[],"empty_obj":{},"i":-3,"n":1.5,"nil":null,"s":"a\"b\né","t":true}"#,
                ),
            ),
            ("scalar_null", json!(null), None),
            ("scalar_true", json!(true), None),
            ("scalar_integer_zero", json!(0), None),
            ("scalar_negative_fraction", json!(-1.25), None),
            ("scalar_string", json!("x"), None),
            ("scalar_empty_array", json!([]), None),
            ("scalar_empty_object", json!({}), None),
        ] {
            let expected = expected.map(str::to_owned).unwrap_or_else(|| {
                serde_json::to_string(&input).unwrap_or_else(|err| panic!("{name}: {err}"))
            });
            assert_eq!(canonical_json(&input), expected, "{name}");
        }
    }
}
