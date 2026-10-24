// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `mutate__edit` — semantic edit + replace-with-expected as a Component Tool (#31).
//!
//! The guest asks `read-for-edit` to validate the planning revision before
//! applying pure semantic ops via `mutate-shared`. Successful compilation then
//! calls `replace-with-expected`, whose final host-side revision check remains
//! the installation authority. Semantic failures re-enter `refuse-edit` so the
//! host preserves their structured CEP class without installing bytes.

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
use bindings::lattice::tool_host::workspace_mutate::{
    self as mutate, EditRefusalKind, MutateError,
};

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
        let file = mutate::read_for_edit(&params.path, &params.expected_hash).map_err(describe)?;
        let after = match apply_edits(&file.content, &params.edits) {
            Ok(after) => after,
            Err(diag) => return Err(describe_diag(&params.path, diag)),
        };

        // Host rechecks expected_hash, records MutationEvidence / denial, and
        // only then installs. Do not skip this import when the guest can see
        // a revision mismatch — that is the host's job.
        let _installed = mutate::replace_with_expected(&params.path, &params.expected_hash, &after)
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

fn describe_diag(path: &str, diag: EditDiag) -> String {
    let kind = if diag.replan {
        EditRefusalKind::Replan
    } else {
        EditRefusalKind::Validation
    };
    describe(mutate::refuse_edit(path, kind, &diag.field, &diag.reason))
}
