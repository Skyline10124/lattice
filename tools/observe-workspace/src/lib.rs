// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `observe.workspace` — Policy-Visible Workspace observation as a Component
//! Tool (#30).
//!
//! This guest owns typed mode dispatch and the outer union result document.
//! Security-sensitive content rendering remains host-owned so the exact bytes
//! visible to the model are the bytes authorized by the Turn budget.
//!
//! # What this Tool cannot do
//!
//! It holds no filesystem path, no preopened directory, no file descriptor, and
//! no handle to a workspace object. Its whole reach is the four functions of the
//! `workspace-observe` import, each of which takes the *requested* path as a
//! string and re-enters the host, which resolves it per segment, applies System
//! Path Exclusion and root containment, consults one Policy-Visible Workspace
//! decision, clamps the caller's limit to policy, charges the Turn budget, and
//! records the concrete path in the call's evidence. None of that is reachable
//! from here, and none of it can be skipped by calling differently: there is no
//! other way in.
//!
//! The import surface is also the *entire* declared surface. A `wasi:*` import
//! would fail the build's import-closure check, and an interface this Tool did
//! not declare in its manifest is absent from the world it compiles against.
//!
//! # Validation is host-side
//!
//! Every input rule lives in the manifest's params schema, so the host refuses a
//! bad call with one structured `SchemaMismatch` before this Component is
//! instantiated. `run` therefore performs no validation of its own: a second,
//! imperative copy of the rules inside the guest could disagree with the
//! descriptor, and the guest is the side of the boundary whose word the host does
//! not take. What remains here is genuinely shaping — and where shaping needs a
//! fact, the fact comes from the host (the content hash, the truncation verdict).
//!
//! # Truncation and hashes are not this Tool's claims
//!
//! `truncated` is echoed from the host's verdict, and `content_hash` is echoed
//! from the host's computation. This Tool cannot produce either value itself, and
//! the audit trail records the host's, so a guest that lied about one would
//! contradict durable evidence rather than hide anything.

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// Generated WIT bindings live in `bindings` (the convention `export_tool!` relies
// on). `wit_bindgen` is a direct dep because the generated `_rt` module
// references `wit_bindgen::rt::...` by bare crate path; the prelude re-export
// pins the version and provides the export glue. `export_tool!` is invoked
// INSIDE `bindings` because wit-bindgen's generated `export!` resolves `self` at
// the call site.
mod bindings {
    lattice_tool_prelude::wit_bindgen::generate!({
        path: "generated",
    });
    use crate::ObserveWorkspace;
    lattice_tool_prelude::export_tool!(ObserveWorkspace);
}

use bindings::exports::lattice::tool_host::tool_api::{Guest, ToolOutput};
use bindings::lattice::tool_host::host_control;
use bindings::lattice::tool_host::workspace_observe::{self as observe, EntryKind, ObserveError};

struct ObserveWorkspace;

/// The Cell every operation and the result share: observation allocates context
/// from a workspace read.
///
/// Trust-free 3-segment CellRef. The host resolves `workspace` against the
/// descriptor and alone appends the trust segment to the audit key; a guest
/// cannot encode trust, because `parse_cell_ref` rejects a fourth segment.
const RESULT_CELL: &str = "alloc:read:workspace";

impl Guest for ObserveWorkspace {
    fn run(params_json: String) -> Result<ToolOutput, String> {
        // Cooperative cancellation: the host also enforces an epoch deadline,
        // and its call-scoped cancellation state — not this string — is what
        // classifies the outcome.
        if host_control::cancelled() {
            return Err(String::from("cancelled"));
        }

        // The host validated `params_json` against the union params schema
        // before this Component was entered, so a parse failure here means the
        // host handed over something its own validator accepted — a host bug,
        // not a caller error. Report it plainly rather than guessing a mode.
        let params: serde_json::Value =
            serde_json::from_str(&params_json).map_err(|err| format!("malformed params: {err}"))?;
        let mode = params
            .get("mode")
            .and_then(|value| value.as_str())
            .ok_or_else(|| String::from("params carry no mode discriminant"))?;

        let value = match mode {
            "read_file" => read_file(&params)?,
            "grep" => grep(&params)?,
            "list_directory" => list_directory(&params)?,
            "find_files" => find_files(&params)?,
            // Unreachable through the schema, which closes `mode` to these four
            // via `const` on each branch. Refused rather than defaulted: a fifth
            // mode arriving here would mean the descriptor and this dispatch
            // disagree, and picking a mode on the guest's initiative is exactly
            // the improvisation the host must not accept.
            other => return Err(format!("unsupported mode: {other}")),
        };

        Ok(ToolOutput {
            result_cell: String::from(RESULT_CELL),
            value_json: value.to_string(),
        })
    }
}

/// Read one file and render the requested line window.
///
/// The host returns the uniquely rendered, already-budgeted content window and
/// its hash. The guest only places those facts in the union result envelope.
fn read_file(params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let path = str_field(params, "path")?;
    let result = observe::read_file(
        path,
        u64_field(params, "offset"),
        u64_field(params, "limit"),
        params.get("line_numbers").and_then(|value| value.as_bool()),
    )
    .map_err(describe)?;
    Ok(serde_json::json!({
        "mode": "read_file",
        "result": {
            "content": result.content,
            // Echoed from the host. This Tool cannot compute it: the value
            // `mutate.edit` verifies must originate host-side or the precondition
            // would be checking the guest against itself.
            "content_hash": result.content_hash,
        }
    }))
}

/// Regex-search visible files beneath a root.
fn grep(params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let pattern = str_field(params, "pattern")?;
    let result = observe::grep(
        pattern,
        params.get("path").and_then(|value| value.as_str()),
        u64_field(params, "max_results"),
    )
    .map_err(describe)?;

    let matches: Vec<serde_json::Value> = result
        .matches
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "path": m.path,
                "line": m.line,
                "text": m.text,
            })
        })
        .collect();
    let mut shaped = serde_json::Map::new();
    shaped.insert("matches".to_string(), serde_json::Value::Array(matches));
    insert_truncated(&mut shaped, result.truncated);
    Ok(serde_json::json!({ "mode": "grep", "result": shaped }))
}

/// List one directory's visible entries.
fn list_directory(params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let path = str_field(params, "path")?;
    let result = observe::list_dir(path, u64_field(params, "limit")).map_err(describe)?;

    let entries: Vec<serde_json::Value> = result
        .entries
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "name": entry.name,
                "kind": entry_kind_label(entry.kind),
            })
        })
        .collect();
    let mut shaped = serde_json::Map::new();
    shaped.insert("entries".to_string(), serde_json::Value::Array(entries));
    insert_truncated(&mut shaped, result.truncated);
    Ok(serde_json::json!({ "mode": "list_directory", "result": shaped }))
}

/// Glob-search visible paths beneath a root.
fn find_files(params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let pattern = str_field(params, "pattern")?;
    let result = observe::find_files(
        pattern,
        params.get("path").and_then(|value| value.as_str()),
        u64_field(params, "limit"),
    )
    .map_err(describe)?;

    let paths: Vec<serde_json::Value> = result
        .paths
        .into_iter()
        .map(serde_json::Value::String)
        .collect();
    let mut shaped = serde_json::Map::new();
    shaped.insert("paths".to_string(), serde_json::Value::Array(paths));
    insert_truncated(&mut shaped, result.truncated);
    Ok(serde_json::json!({ "mode": "find_files", "result": shaped }))
}

/// Carry the host's truncation verdict into the result document.
///
/// Present only when the host cut something, matching the native wire shape this
/// migration preserves. The verdict itself is never recomputed here — the host's
/// evidence record is the audit trail's answer, and this field is its echo.
fn insert_truncated(shaped: &mut serde_json::Map<String, serde_json::Value>, truncated: bool) {
    if truncated {
        shaped.insert("truncated".to_string(), serde_json::Value::Bool(true));
    }
}

/// The four directory-entry kinds, spelled as the result schema's enum.
fn entry_kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "file",
        EntryKind::Directory => "directory",
        EntryKind::Symlink => "symlink",
        EntryKind::Other => "other",
    }
}

/// Turn a host refusal into the guest's error string.
///
/// The host already decided what a caller may learn — a hidden path and a missing
/// one arrive here identically as `not-found`, with the policy reason recorded in
/// evidence instead — so this only unwraps the message it was given. Nothing is
/// added, because anything this Tool could add would be its own inference about a
/// boundary it cannot see.
fn describe(err: ObserveError) -> String {
    match err {
        ObserveError::NotFound(message)
        | ObserveError::PathRejected(message)
        | ObserveError::LimitExceeded(message)
        | ObserveError::BudgetExhausted(message)
        | ObserveError::Denied(message)
        | ObserveError::InvalidRequest(message)
        | ObserveError::IoError(message) => message,
    }
}

/// A required string field the schema guarantees is present.
fn str_field<'a>(params: &'a serde_json::Value, field: &str) -> Result<&'a str, String> {
    params
        .get(field)
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("params carry no {field}"))
}

/// An optional unsigned field. Absent and non-integer collapse to `None`, which
/// the host reads as "use the policy default" — the guest never invents a bound.
fn u64_field(params: &serde_json::Value, field: &str) -> Option<u64> {
    params.get(field).and_then(|value| value.as_u64())
}
