// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `noop.echo` — the tracer Source Tool guest (ADR 0004 / #5 slice B).
//!
//! Exercises the Tool Host control surface (cancellation probe + structured
//! progress event) and echoes its `message` param back as a structured
//! `ToolOutput`. It declares one `session/in` result capability but performs
//! no workspace, network, or session domain operation — those arrive with #6
//! capability admission. The import whitelist is exactly `host-control`.

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::str;

// Generated WIT bindings live in `bindings` (the convention `export_tool!`
// relies on). `wit_bindgen` is a direct dep because the generated `_rt`
// module references `wit_bindgen::rt::...` by bare crate path; the prelude
// re-export pins the version and provides the export glue. `export_tool!`
// is invoked INSIDE `bindings` because wit-bindgen's generated `export!`
// resolves `self` at the call site.
mod bindings {
    lattice_tool_prelude::wit_bindgen::generate!({
        path: "generated",
    });
    use crate::NoopEcho;
    lattice_tool_prelude::export_tool!(NoopEcho);
}

use bindings::exports::lattice::tool_host::tool_api::{Guest, ToolOutput};
use bindings::lattice::tool_host::host_control;

struct NoopEcho;

impl Guest for NoopEcho {
    fn run(params_json: String) -> Result<ToolOutput, String> {
        // Exercise host control; no domain operation is available before #6.
        if host_control::cancelled() {
            return Err(String::from("cancelled"));
        }
        host_control::emit_event("{\"phase\":\"probed\"}");

        let message = extract_message(&params_json)?;
        Ok(ToolOutput {
            // Trust-free 3-segment CellRef; the host resolves `session` in the
            // descriptor and alone appends `:in` to the audit key. A guest
            // cannot encode trust (parse_cell_ref rejects a 4th segment).
            result_cell: String::from("alloc:none:session"),
            value_json: format!("{{\"echoed\":{}}}", json_string(&message)),
        })
    }
}

/// Extract the `message` string from the params JSON without a JSON library
/// (the guest is `#![no_std]` and must not pull a host JSON dep). The params
/// schema guarantees a single `message` string field.
fn extract_message(params_json: &str) -> Result<String, String> {
    // Minimal, schema-driven extraction: find `"message":"..."` (or with
    // optional whitespace). The schema forbids additional properties, so this
    // is sufficient for the tracer. Real Tools get a host-side JSON decode.
    let key = "\"message\"";
    let key_pos = params_json
        .find(key)
        .ok_or_else(|| String::from("missing message field"))?;
    let after_key = &params_json[key_pos + key.len()..];
    let after_colon = after_key
        .trim_start()
        .strip_prefix(':')
        .ok_or_else(|| String::from("missing colon after message"))?
        .trim_start();
    let after_quote = after_colon
        .strip_prefix('"')
        .ok_or_else(|| String::from("message is not a string"))?;
    // Scan for the closing quote, handling the minimal escape set.
    let mut out = String::new();
    let mut chars = after_quote.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            return Ok(out);
        }
        if c == '\\' {
            let esc = chars
                .next()
                .ok_or_else(|| String::from("truncated escape"))?;
            match esc {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            }
            continue;
        }
        out.push(c);
    }
    Err(String::from("unterminated message string"))
}

/// Encode a string as a JSON string literal body (with surrounding quotes).
fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_message_round_trip() {
        let p = r#"{"message":"hello"}"#;
        assert_eq!(extract_message(p).unwrap(), "hello");
    }

    #[test]
    fn extract_message_handles_escapes() {
        let p = r#"{"message":"a\"b\\c"}"#;
        assert_eq!(extract_message(p).unwrap(), "a\"b\\c");
    }

    #[test]
    fn extract_message_rejects_missing_field() {
        assert!(extract_message(r#"{"other":"x"}"#).is_err());
    }

    #[test]
    fn json_string_escapes() {
        assert_eq!(json_string("a\"b"), r#""a\"b""#);
        assert_eq!(json_string("\n"), r#""\n""#);
    }
}
