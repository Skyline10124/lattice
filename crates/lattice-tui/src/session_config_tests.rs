// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lattice_api::local::{LocalError, LocalErrorKind};

#[tokio::test]
async fn preflight_and_submission_preserve_the_same_profile_rejection() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join(".lattice/agents/selected/agent.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    let client = LocalClient::new(lattice_api::StateLayout::in_state_root(root.path()));
    let mut options = TuiOptions {
        session_id: None,
        agent: "selected".into(),
        session_dir: root.path().join("sessions"),
        agents_dir: std::path::PathBuf::new(),
        project_root: root.path().into(),
    };
    for (agents_dir, source, expected) in [
        ("/etc/agents", "", "LATTICE_AGENTS_DIR"),
        (
            "",
            "schema_version=1\n[agent]\nname='sk-private-name-a932'\nmodel='sonnet'\n[prompt]\nsystem='hi'",
            "must match selected agent 'selected'",
        ),
        ("", "schema_version=@sk-private-name-a932", "line 1"),
    ] {
        options.agents_dir = agents_dir.into();
        std::fs::write(&profile, source).unwrap();
        let preflight = open_or_create(&options, &client)
            .await
            .err()
            .expect("reject preflight");
        let error = preflight
            .downcast_ref::<lattice_api::ApiError>()
            .unwrap()
            .clone();
        let preflight = LocalError::from(error);
        let submission = freeze_configuration(options.agent.clone(), options.clone())
            .await
            .err()
            .expect("reject submission");
        assert_eq!(preflight.kind, LocalErrorKind::BadRequest);
        assert_eq!(preflight.kind, submission.kind);
        assert_eq!(preflight.message, submission.message);
        assert!(preflight.message.contains(expected));
        assert!(!preflight.message.contains("sk-private-name-a932"));
    }
}
