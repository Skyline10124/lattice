// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lattice_api::local::{LocalError, LocalErrorKind};

#[tokio::test]
async fn preflight_and_submission_preserve_the_same_profile_rejection() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join(".lattice/agents/selected/agent.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::set_permissions(
        root.path().join(".lattice"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .unwrap();
    let layout = lattice_api::StateLayout::in_state_root(root.path());
    let client = LocalClient::new(layout.clone());
    let home = root.path().to_owned();
    let server = tokio::spawn(lattice_api::local::serve(layout, Some(home)));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    client
        .auth_set("openai", "short.fixture-key".into())
        .await
        .unwrap();
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
            "schema_version=1\n[agent]\nname='sk-private-name-a932'\nmodel='anthropic/claude-sonnet-4-6'\n[prompt]\nsystem='hi'",
            "must match selected agent 'selected'",
        ),
        ("", "schema_version=@sk-private-name-a932", "line 1"),
        (
            "",
            "schema_version=1\n[agent]\nname='different'\nmodel='anthropic/claude-sonnet-4-6'\n[prompt]\nsystem='hi'",
            "declares 'different'",
        ),
    ] {
        options.agents_dir = agents_dir.into();
        std::fs::write(&profile, source).unwrap();
        let preflight = open_or_create(&options, &client)
            .await
            .err()
            .expect("reject preflight");
        let preflight = preflight.downcast_ref::<LocalError>().unwrap().clone();
        let submission =
            freeze_configuration(options.agent.clone(), options.clone(), client.clone())
                .await
                .err()
                .expect("reject submission");
        assert_eq!(preflight.kind, LocalErrorKind::BadRequest);
        assert_eq!(preflight.kind, submission.kind);
        assert_eq!(preflight.message, submission.message);
        assert!(preflight.message.contains(expected));
        assert!(!preflight.message.contains("sk-private-name-a932"));
    }
    std::fs::write(&profile, "schema_version=1\n[agent]\nname='short.fixture-key'\nmodel='local/test'\n[prompt]\nsystem='hi'").unwrap();
    let error = freeze_configuration(options.agent.clone(), options.clone(), client.clone())
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("declares '[REDACTED]'"));
    assert!(!error.message.contains("short.fixture-key"));
    let long_key = "ordinary.secret.fragment.".repeat(100);
    client.auth_set("openai", long_key.clone()).await.unwrap();
    std::fs::write(&profile, format!("schema_version=1\n[agent]\nname='{long_key}'\nmodel='local/test'\n[prompt]\nsystem='hi'")).unwrap();
    let preflight = open_or_create(&options, &client).await.err().unwrap();
    let preflight = preflight.downcast_ref::<LocalError>().unwrap();
    let submission = freeze_configuration(options.agent.clone(), options.clone(), client.clone())
        .await
        .err()
        .unwrap();
    for error in [preflight, &submission] {
        assert!(error.message.contains("declares '[REDACTED]'"));
        assert!(!error.message.contains(&long_key));
        assert!(!error.message.contains("ordinary.secret.fragment"));
    }
    client.stop().await.unwrap();
    server.await.unwrap().unwrap();
}
