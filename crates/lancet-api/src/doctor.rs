// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET doctor API for frontends.

use std::path::PathBuf;

use lancet_catalog::CATALOG_LEN;
use lancet_catalog::catalog::catalog;
use lancet_native::{MethodRegistry, register_production_method_registry};

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

/// Run LANCET doctor checks and return structured frontend data.
pub fn run_doctor_checks(request: LancetDoctorRequest) -> LancetDoctorOutput {
    let _ = &request.project_root;
    let mut ok_items: Vec<String> = Vec::new();
    let warnings: Vec<String> = Vec::new();
    let mut issues: Vec<String> = Vec::new();

    ok_items.push("doctor mode: informational (exit 0)".to_string());

    let catalog_len = catalog().len();
    if catalog_len == CATALOG_LEN {
        ok_items.push(format!(
            "catalog: {catalog_len} methods (expected {CATALOG_LEN})"
        ));
    } else {
        issues.push(format!(
            "catalog: expected {CATALOG_LEN} methods, got {catalog_len}"
        ));
    }

    {
        let mut registry = MethodRegistry::new();
        match register_production_method_registry(&mut registry) {
            Ok(()) => ok_items.push("registry: production methods registered".to_string()),
            Err(err) => issues.push(format!("registry: {}", err.message)),
        }
    }

    LancetDoctorOutput {
        ok_items,
        warnings,
        issues,
    }
}
