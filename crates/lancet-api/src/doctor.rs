// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET doctor API for frontends.

use std::path::PathBuf;

use lancet_runtime::ComponentRuntime;

/// Request shape for LANCET doctor checks.
#[derive(Clone, Debug)]
pub struct LancetDoctorRequest {
    pub project_root: PathBuf,
}

impl LancetDoctorRequest {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }
}

/// Structured doctor result for CLI/TUI/GUI frontends.
#[derive(Clone, Debug)]
pub struct LancetDoctorOutput {
    pub ok_items: Vec<String>,
    pub warnings: Vec<String>,
    pub issues: Vec<String>,
}

/// Validate the canonical workspace and generated closed Component catalog.
pub fn run_doctor_checks(request: LancetDoctorRequest) -> LancetDoctorOutput {
    let mut ok_items = Vec::new();
    let warnings = Vec::new();
    let mut issues = Vec::new();

    match std::fs::canonicalize(&request.project_root) {
        Ok(root) => ok_items.push(format!("workspace: {}", root.display())),
        Err(error) => issues.push(format!(
            "workspace: cannot canonicalize '{}': {error}",
            request.project_root.display()
        )),
    }

    match ComponentRuntime::shared() {
        Ok(runtime) => ok_items.push(format!(
            "component catalog: {} generated tools composed",
            runtime.descriptors().count()
        )),
        Err(error) => issues.push(format!("component catalog: {error}")),
    }

    LancetDoctorOutput {
        ok_items,
        warnings,
        issues,
    }
}
