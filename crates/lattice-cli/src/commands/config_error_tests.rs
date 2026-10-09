// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn configuration_errors_keep_reason_and_exit_two_through_anyhow() {
    let error = lattice_api::validate_agents_dir(Path::new("/etc/agents")).unwrap_err();
    let local = lattice_api::local::LocalError::from(error.clone());
    for error in [
        anyhow::Error::new(error).context("TUI startup preflight"),
        anyhow::Error::new(local.clone()).context("TUI configuration"),
        submission_error(local.clone()),
    ] {
        let (code, record) = error_record(&error);
        assert_eq!(code, 2);
        assert_eq!(record["type"], "error");
        assert_eq!(record["kind"], "bad_request");
        assert_eq!(record["message"], local.message);
        assert!(
            record["message"]
                .as_str()
                .unwrap()
                .contains("LATTICE_AGENTS_DIR")
        );
    }
    let error = anyhow::Error::new(lattice_api::ApiError::BadRequest("sk-private-input".into()));
    let (code, record) = error_record(&error);
    assert_eq!(code, 2);
    assert_eq!(
        record["message"],
        "request or frozen configuration is invalid"
    );
}
