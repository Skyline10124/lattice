// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `mutate__create` — create-if-absent install as a Component Tool (#31).
//!
//! The guest holds no filesystem path and no write handle. Its whole reach is
//! the `workspace-mutate` import: `create-if-absent` re-enters the host, which
//! resolves the path, authorizes `mutate:write:workspace`, enforces O_EXCL /
//! WriteLimits, appends private history, and records MutationEvidence.
//!
//! Input validation lives in the manifest params schema. `run` performs no
//! second imperative copy of those rules.

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

mod bindings {
    lattice_tool_prelude::wit_bindgen::generate!({
        path: "generated",
    });
    use crate::MutateCreate;
    lattice_tool_prelude::export_tool!(MutateCreate);
}

use bindings::exports::lattice::tool_host::tool_api::{Guest, ToolOutput};
use bindings::lattice::tool_host::host_control;
use bindings::lattice::tool_host::workspace_mutate::{self as mutate, MutateError};

struct MutateCreate;

/// Result cell: the returned create receipt enters context as a workspace write
/// allocation. Operation authorization uses `mutate:write:workspace` on the host.
const RESULT_CELL: &str = "alloc:write:workspace";

impl Guest for MutateCreate {
    fn run(params_json: String) -> Result<ToolOutput, String> {
        if host_control::cancelled() {
            return Err(String::from("cancelled"));
        }

        let params: serde_json::Value =
            serde_json::from_str(&params_json).map_err(|err| format!("malformed params: {err}"))?;
        let path = str_field(&params, "path")?;
        let content = str_field(&params, "content")?;
        let create_parents = params
            .get("create_parents")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        // Always call the host — do not short-circuit on a guest-side existence
        // guess. Precondition, evidence, and atomic install are host-owned.
        let installed =
            mutate::create_if_absent(path, content, create_parents).map_err(describe)?;

        let mut result = serde_json::Map::new();
        result.insert("path".into(), serde_json::Value::String(installed.path));
        result.insert(
            "bytes_written".into(),
            serde_json::Value::from(installed.bytes_written),
        );
        if !installed.created_dirs.is_empty() {
            result.insert(
                "created_dirs".into(),
                serde_json::Value::Array(
                    installed
                        .created_dirs
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect::<Vec<_>>(),
                ),
            );
        }

        Ok(ToolOutput {
            result_cell: String::from(RESULT_CELL),
            value_json: serde_json::Value::Object(result).to_string(),
        })
    }
}

fn describe(err: MutateError) -> String {
    match err {
        MutateError::NotFound(message)
        | MutateError::AlreadyExists(message)
        | MutateError::StaleRevision(message)
        | MutateError::SemanticReplan(message)
        | MutateError::Validation(message)
        | MutateError::PathRejected(message)
        | MutateError::Denied(message)
        | MutateError::LimitExceeded(message)
        | MutateError::Cancelled(message)
        | MutateError::IoError(message)
        | MutateError::InvalidRequest(message) => message,
    }
}

fn str_field<'a>(params: &'a serde_json::Value, field: &str) -> Result<&'a str, String> {
    params
        .get(field)
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("params missing string field `{field}`"))
}
