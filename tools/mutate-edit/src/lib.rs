// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `mutate.edit` — semantic edit + replace-with-expected as a Component Tool (#31).
//!
//! The guest reads bytes through `read-for-edit`, applies pure semantic ops via
//! `mutate-shared`, then always calls `replace-with-expected` — even when the
//! guest can see that `expected_hash` does not match the host revision. Short-
//! circuiting on the guest would skip host precondition enforcement and skip
//! stale MutationEvidence (Standards #2).

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use mutate_shared::{EditDiag, EditOp, apply_edits};
use serde::Deserialize;

mod bindings {
    lattice_tool_prelude::wit_bindgen::generate!({
        path: "generated",
    });
    use crate::MutateEdit;
    lattice_tool_prelude::export_tool!(MutateEdit);
}

use bindings::exports::lattice::tool_host::tool_api::{Guest, ToolOutput};
use bindings::lattice::tool_host::host_control;
use bindings::lattice::tool_host::workspace_mutate::{self as mutate, MutateError};

struct MutateEdit;

const RESULT_CELL: &str = "alloc:write:workspace";

#[derive(Deserialize)]
struct EditParams {
    path: String,
    expected_hash: String,
    edits: Vec<EditOp>,
}

impl Guest for MutateEdit {
    fn run(params_json: String) -> Result<ToolOutput, String> {
        if host_control::cancelled() {
            return Err(String::from("cancelled"));
        }

        let params: EditParams =
            serde_json::from_str(&params_json).map_err(|err| format!("malformed params: {err}"))?;

        // Always ask the host for authoritative bytes + Local Revision.
        let file = mutate::read_for_edit(&params.path).map_err(describe)?;
        let after = apply_edits(&file.content, &params.edits).map_err(describe_diag)?;

        // Host rechecks expected_hash, records MutationEvidence / denial, and
        // only then installs. Do not compare revisions here.
        let _installed =
            mutate::replace_with_expected(&params.path, &params.expected_hash, &after)
                .map_err(describe)?;

        let value = serde_json::json!({
            "changed_files": vec![params.path],
        });

        Ok(ToolOutput {
            result_cell: String::from(RESULT_CELL),
            value_json: value.to_string(),
        })
    }
}

fn describe(err: MutateError) -> String {
    match err {
        MutateError::NotFound(message)
        | MutateError::AlreadyExists(message)
        | MutateError::StaleRevision(message)
        | MutateError::PathRejected(message)
        | MutateError::Denied(message)
        | MutateError::LimitExceeded(message)
        | MutateError::Cancelled(message)
        | MutateError::IoError(message)
        | MutateError::InvalidRequest(message) => message,
    }
}

fn describe_diag(diag: EditDiag) -> String {
    // Field-tagged validation mirrors native SchemaMismatch text; replan
    // diagnostics mirror ReplanRequired reason strings.
    if diag.replan {
        diag.reason
    } else {
        format!("{}: {}", diag.field, diag.reason)
    }
}
