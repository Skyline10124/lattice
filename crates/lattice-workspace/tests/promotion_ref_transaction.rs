// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Atomic three-ref promotion transaction coverage.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use lattice_workspace::promotion::{
    PromotionRefs, RefComponent, RefTransactionError, RefTransactionRequest,
    build_update_ref_z_payload, run_ref_transaction,
};

fn init_repo(dir: &Path, branch: &str) -> String {
    Command::new("git")
        .args(["init", "-b", branch])
        .current_dir(dir)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@localhost"])
        .current_dir(dir)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(dir)
        .output()
        .expect("git config name");
    std::fs::write(dir.join("README.md"), b"init\n").expect("write readme");
    Command::new("git")
        .args(["add", "README.md"])
        .current_dir(dir)
        .output()
        .expect("git add");
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .expect("git commit");
    git_rev_parse(dir, "HEAD")
}

fn git_rev_parse(dir: &Path, spec: &str) -> String {
    String::from_utf8(
        Command::new("git")
            .args(["rev-parse", spec])
            .current_dir(dir)
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_owned()
}

fn commit_tree(dir: &Path, parent: Option<&str>, message: &str) -> String {
    let tree = git_rev_parse(dir, "HEAD^{tree}");
    let mut args = vec!["commit-tree", &tree, "-m", message];
    if let Some(parent) = parent {
        args.push("-p");
        args.push(parent);
    }
    String::from_utf8(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("commit-tree")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_owned()
}

fn snapshot_refs(dir: &Path, refs: &PromotionRefs) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for name in [
        refs.project_ref.as_str(),
        refs.session_ref.as_str(),
        refs.candidate_ref.as_str(),
    ] {
        let output = Command::new("git")
            .args(["rev-parse", name])
            .current_dir(dir)
            .output()
            .expect("rev-parse");
        if output.status.success() {
            map.insert(
                name.to_owned(),
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            );
        }
    }
    map
}

fn fixture_refs() -> PromotionRefs {
    PromotionRefs::new(
        "main",
        &RefComponent::parse("session_id", "sess1").expect("session"),
        &RefComponent::parse("turn_id", "turn1").expect("turn"),
    )
    .expect("refs")
}

fn sample_request(
    refs: &PromotionRefs,
    old_project: &str,
    old_session: Option<String>,
    candidate: &str,
    session: &str,
) -> RefTransactionRequest {
    RefTransactionRequest::new(refs.clone(), old_project, old_session, candidate, session)
        .expect("request")
}

mod ref_protocol {
    use super::*;

    #[test]
    fn golden_payload_has_one_transaction_cycle_and_three_commands() {
        let refs = fixture_refs();
        let request = sample_request(
            &refs,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            "cccccccccccccccccccccccccccccccccccccccc",
            "dddddddddddddddddddddddddddddddddddddddd",
        );
        let payload = build_update_ref_z_payload(&request).expect("payload");

        let parts: Vec<&[u8]> = payload
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .collect();
        assert_eq!(parts[0], b"start");
        assert_eq!(
            parts[1],
            b"update refs/heads/main".as_ref(),
            "project update command"
        );
        assert_eq!(parts[2], b"cccccccccccccccccccccccccccccccccccccccc");
        assert_eq!(parts[3], b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(
            parts[4],
            b"update refs/lattice/sessions/sess1/main".as_ref()
        );
        assert_eq!(parts[5], b"dddddddddddddddddddddddddddddddddddddddd");
        assert_eq!(parts[6], b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(
            parts[7],
            b"create refs/lattice/candidates/sess1/turn1".as_ref()
        );
        assert_eq!(parts[8], b"cccccccccccccccccccccccccccccccccccccccc");
        assert_eq!(parts[9], b"prepare");
        assert_eq!(parts[10], b"commit");
        assert_eq!(parts.len(), 11, "unexpected extra protocol fields");
    }

    #[test]
    fn first_session_commit_uses_create_not_update() {
        let refs = fixture_refs();
        let request = sample_request(
            &refs,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
            "cccccccccccccccccccccccccccccccccccccccc",
            "dddddddddddddddddddddddddddddddddddddddd",
        );
        let payload = build_update_ref_z_payload(&request).expect("payload");
        let text = String::from_utf8_lossy(&payload);
        assert!(text.contains("create refs/lattice/sessions/sess1/main"));
        assert!(!text.contains("update refs/lattice/sessions/sess1/main"));
    }

    #[test]
    fn injection_corpus_rejects_before_git_spawn() {
        let corpus: &[(&str, &str)] = &[
            ("session_id", "bad\nid"),
            ("session_id", "bad\0id"),
            ("turn_id", "-leading"),
            ("turn_id", "has..dots"),
            ("turn_id", "has@{curl"),
            ("turn_id", "back\\slash"),
            ("branch", ""),
            ("branch", "-bad"),
            ("branch", "bad..branch"),
            ("branch", "bad@{branch"),
            ("branch", "bad\\branch"),
            ("session_id", ""),
        ];

        for (field, value) in corpus {
            let err = if *field == "branch" {
                PromotionRefs::new(
                    value,
                    &RefComponent::parse("session_id", "sess1").unwrap_or_else(|_| {
                        RefComponent::parse("session_id", "x").expect("fallback")
                    }),
                    &RefComponent::parse("turn_id", "turn1").expect("turn"),
                )
                .err()
            } else {
                RefComponent::parse(field, value).err()
            };
            assert!(err.is_some(), "expected rejection for {field}={value:?}");
        }
    }

    #[test]
    fn derived_refs_match_d07_layout() {
        let refs = fixture_refs();
        assert_eq!(refs.project_ref.as_str(), "refs/heads/main");
        assert_eq!(
            refs.session_ref.as_str(),
            "refs/lattice/sessions/sess1/main"
        );
        assert_eq!(
            refs.candidate_ref.as_str(),
            "refs/lattice/candidates/sess1/turn1"
        );
    }
}

mod ref_transaction {
    use super::*;

    #[test]
    fn success_moves_all_three_refs_together() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = init_repo(dir.path(), "main");
        let candidate = commit_tree(dir.path(), Some(&base), "candidate");
        let session = commit_tree(dir.path(), None, "session");
        let refs = fixture_refs();
        let request = sample_request(&refs, &base, None, &candidate, &session);

        let outcome = run_ref_transaction(dir.path(), &request).expect("transaction");
        assert_eq!(outcome.project_oid, candidate);
        assert_eq!(outcome.session_oid, session);
        assert_eq!(outcome.candidate_oid, candidate);
    }

    #[test]
    fn stale_project_oid_leaves_refs_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = init_repo(dir.path(), "main");
        let candidate = commit_tree(dir.path(), Some(&base), "candidate");
        let session = commit_tree(dir.path(), None, "session");
        let refs = fixture_refs();
        let before = snapshot_refs(dir.path(), &refs);

        let request = sample_request(
            &refs,
            "0000000000000000000000000000000000000000",
            None,
            &candidate,
            &session,
        );
        assert!(run_ref_transaction(dir.path(), &request).is_err());
        assert_eq!(snapshot_refs(dir.path(), &refs), before);
    }

    #[test]
    fn stale_session_oid_leaves_refs_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = init_repo(dir.path(), "main");
        let candidate = commit_tree(dir.path(), Some(&base), "candidate");
        let session = commit_tree(dir.path(), None, "session");
        let refs = fixture_refs();

        let first_session = commit_tree(dir.path(), None, "first-session");
        let seed = sample_request(&refs, &base, None, &base, &first_session);
        run_ref_transaction(dir.path(), &seed).expect("seed session ref");
        let before = snapshot_refs(dir.path(), &refs);

        let request = sample_request(
            &refs,
            &base,
            Some("0000000000000000000000000000000000000000".into()),
            &candidate,
            &session,
        );
        assert!(run_ref_transaction(dir.path(), &request).is_err());
        assert_eq!(snapshot_refs(dir.path(), &refs), before);
    }

    #[test]
    fn existing_candidate_ref_blocks_whole_transaction() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = init_repo(dir.path(), "main");
        let candidate = commit_tree(dir.path(), Some(&base), "candidate");
        let session = commit_tree(dir.path(), None, "session");
        let refs = fixture_refs();

        Command::new("git")
            .args(["update-ref", refs.candidate_ref.as_str(), &candidate])
            .current_dir(dir.path())
            .output()
            .expect("seed candidate ref");

        let before = snapshot_refs(dir.path(), &refs);
        let request = sample_request(&refs, &base, None, &candidate, &session);
        assert!(run_ref_transaction(dir.path(), &request).is_err());
        assert_eq!(snapshot_refs(dir.path(), &refs), before);
    }

    #[test]
    fn project_ref_moves_between_request_and_execution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = init_repo(dir.path(), "main");
        let candidate = commit_tree(dir.path(), Some(&base), "candidate");
        let session = commit_tree(dir.path(), None, "session");
        let refs = fixture_refs();
        let request = sample_request(&refs, &base, None, &candidate, &session);

        let moved = commit_tree(dir.path(), Some(&base), "moved");
        Command::new("git")
            .args(["update-ref", "refs/heads/main", &moved, &base])
            .current_dir(dir.path())
            .output()
            .expect("move project ref");

        assert!(run_ref_transaction(dir.path(), &request).is_err());
        let after = snapshot_refs(dir.path(), &refs);
        assert_eq!(after.get(refs.project_ref.as_str()), Some(&moved));
        assert!(!after.contains_key(refs.session_ref.as_str()));
        assert!(!after.contains_key(refs.candidate_ref.as_str()));
    }

    #[test]
    fn session_ref_moves_between_request_and_execution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = init_repo(dir.path(), "main");
        let candidate = commit_tree(dir.path(), Some(&base), "candidate");
        let session = commit_tree(dir.path(), None, "session");
        let refs = fixture_refs();

        let first_session = commit_tree(dir.path(), None, "first-session");
        let seed = sample_request(&refs, &base, None, &base, &first_session);
        run_ref_transaction(dir.path(), &seed).expect("seed session ref");
        let before = snapshot_refs(dir.path(), &refs);

        let request = sample_request(
            &refs,
            &base,
            Some(first_session.clone()),
            &candidate,
            &session,
        );

        let moved_session = commit_tree(dir.path(), Some(&first_session), "moved-session");
        Command::new("git")
            .args([
                "update-ref",
                refs.session_ref.as_str(),
                &moved_session,
                &first_session,
            ])
            .current_dir(dir.path())
            .output()
            .expect("move session ref");

        assert!(run_ref_transaction(dir.path(), &request).is_err());
        let after = snapshot_refs(dir.path(), &refs);
        assert_eq!(after.get(refs.project_ref.as_str()), Some(&base));
        assert_eq!(after.get(refs.session_ref.as_str()), Some(&moved_session));
        assert_eq!(
            after.get(refs.candidate_ref.as_str()),
            before.get(refs.candidate_ref.as_str())
        );
    }

    #[test]
    fn child_failure_surfaces_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        let refs = fixture_refs();
        let request = sample_request(
            &refs,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "cccccccccccccccccccccccccccccccccccccccc",
        );
        let err = run_ref_transaction(dir.path(), &request).expect_err("missing objects");
        assert!(matches!(err, RefTransactionError::GitUpdateRef { .. }));
    }
}
