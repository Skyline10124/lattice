// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::super::overlay::OverlayMode;
use super::super::snapshot::capture_lower_baseline;
use super::*;
use crate::edit::hash::line_hash;
use crate::edit::types::HashLine;
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

fn empty_baseline() -> LowerBaseline {
    LowerBaseline {
        entries: BTreeMap::new(),
        regular_file_contents: BTreeMap::new(),
    }
}

fn regular_manifest(path: &str) -> ArtifactManifest {
    ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: path.to_string(),
            operation: ArtifactOperation::Create,
            content_hash: "blake3:test".to_string(),
            size_bytes: 4,
            mode: 0o644,
            lower_hash: None,
        })],
    }
}

fn hash_content(content: &[u8]) -> String {
    format!("{HASH_PREFIX}{}", blake3::hash(content).to_hex())
}

fn anchor(line_number: usize, text: &str) -> HashLine {
    HashLine {
        hash: line_hash(line_number, text),
        text: text.to_string(),
    }
}

fn successful_artifact_cleanup() -> OverlayCleanupReport {
    OverlayCleanupReport {
        mode: OverlayMode::Artifact,
        disposition: OverlayDisposition::Committed,
        outcome: CleanupOutcome::Succeeded,
        failure_kind: None,
        path_label: None,
        reason: None,
        cleanup_failures: Vec::new(),
        child_cleanup_succeeded: true,
    }
}

fn successful_verify_cleanup() -> VerifyDiscardReport {
    VerifyDiscardReport {
        verify_layer: VerifyLayerStatus::Discarded,
        cleanup_outcome: Some(CleanupOutcome::Succeeded),
        failure_kind: None,
        path_label: None,
        reason: None,
        child_cleanup_succeeded: true,
        contaminated_paths: Vec::new(),
    }
}

fn mutation_region(root: &Path, prefix: &str) -> AuthorizedEffectRegion {
    AuthorizedEffectRegion {
        kinds: BTreeSet::from([ImpactKind::WorkspaceMutation]),
        write_paths: Vec::new(),
        write_path_prefixes: vec![root.join(prefix)],
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: false,
    }
}

#[test]
fn artifact_manifest_scans_regular_create_hash_mode_and_order() {
    let upper = TempDir::new().unwrap();
    std::fs::create_dir_all(upper.path().join("b")).unwrap();
    std::fs::write(upper.path().join("z.txt"), "z").unwrap();
    std::fs::write(upper.path().join("a.txt"), "a").unwrap();
    std::fs::write(upper.path().join("b/nested.txt"), "nested").unwrap();

    let report = scan_overlay_upper(upper.path(), &empty_baseline()).unwrap();

    assert!(report.rejected_kinds.is_empty());
    assert_eq!(
        report.manifest.paths(),
        vec!["a.txt", "b", "b/nested.txt", "z.txt"]
    );
    let ArtifactEntry::Regular(entry) = &report.manifest.entries[0] else {
        panic!("expected regular entry");
    };
    assert_eq!(entry.operation, ArtifactOperation::Create);
    assert_eq!(entry.size_bytes, 1);
    assert!(entry.content_hash.starts_with(HASH_PREFIX));
    assert_eq!(entry.mode & 0o777, entry.mode);
}

#[test]
fn artifact_manifest_records_regular_update_lower_hash_and_type_changes() {
    let lower = TempDir::new().unwrap();
    std::fs::write(lower.path().join("updated.txt"), "old").unwrap();
    std::fs::create_dir(lower.path().join("type-change")).unwrap();
    let baseline = capture_lower_baseline(lower.path()).unwrap();

    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("updated.txt"), "new").unwrap();
    std::fs::write(upper.path().join("type-change"), "file").unwrap();

    let report = scan_overlay_upper(upper.path(), &baseline).unwrap();

    let updated = report
        .manifest
        .entries
        .iter()
        .find(|entry| entry.path() == "updated.txt")
        .unwrap();
    let ArtifactEntry::Regular(updated) = updated else {
        panic!("expected regular update");
    };
    assert_eq!(updated.operation, ArtifactOperation::Update);
    assert_eq!(
        updated.lower_hash.as_ref(),
        baseline.entries["updated.txt"].content_hash.as_ref()
    );

    assert_eq!(
        report
            .manifest
            .entries
            .iter()
            .find(|entry| entry.path() == "type-change")
            .unwrap()
            .operation(),
        ArtifactOperation::Update
    );
}

#[cfg(unix)]
#[test]
fn artifact_manifest_records_directory_and_symlink_creates_without_following_symlink() {
    let upper = TempDir::new().unwrap();
    std::fs::create_dir(upper.path().join("dir")).unwrap();
    symlink("/definitely/not/read", upper.path().join("link")).unwrap();

    let report = scan_overlay_upper(upper.path(), &empty_baseline()).unwrap();

    assert!(matches!(
        &report.manifest.entries[0],
        ArtifactEntry::Directory(DirectoryArtifactEntry {
            path,
            operation: ArtifactOperation::Create,
            ..
        }) if path == "dir"
    ));
    assert!(matches!(
        &report.manifest.entries[1],
        ArtifactEntry::Symlink(SymlinkArtifactEntry {
            path,
            operation: ArtifactOperation::Create,
            target,
        }) if path == "link" && target == "/definitely/not/read"
    ));
}

#[test]
fn artifact_manifest_records_marker_whiteout_delete_with_lower_kind_and_hash() {
    let lower = TempDir::new().unwrap();
    std::fs::write(lower.path().join("deleted.txt"), "delete me").unwrap();
    let baseline = capture_lower_baseline(lower.path()).unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join(".wh.deleted.txt"), "").unwrap();

    let report = scan_overlay_upper(upper.path(), &baseline).unwrap();

    assert_eq!(report.manifest.entries.len(), 1);
    let ArtifactEntry::Delete(entry) = &report.manifest.entries[0] else {
        panic!("expected delete entry");
    };
    assert_eq!(entry.path, "deleted.txt");
    assert_eq!(entry.operation, ArtifactOperation::Delete);
    assert_eq!(entry.lower_kind, Some(ArtifactFileKind::RegularFile));
    assert_eq!(
        entry.lower_hash.as_ref(),
        baseline.entries["deleted.txt"].content_hash.as_ref()
    );
}

#[test]
fn artifact_manifest_records_opaque_marker_deletes_for_lower_descendants() {
    let lower = TempDir::new().unwrap();
    std::fs::create_dir(lower.path().join("dir")).unwrap();
    std::fs::write(lower.path().join("dir/old.txt"), "old").unwrap();
    let baseline = capture_lower_baseline(lower.path()).unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::create_dir(upper.path().join("dir")).unwrap();
    std::fs::write(upper.path().join("dir/.wh..wh..opq"), "").unwrap();

    let report = scan_overlay_upper(upper.path(), &baseline).unwrap();

    assert!(report.manifest.entries.iter().any(
        |entry| matches!(entry, ArtifactEntry::Delete(delete) if delete.path == "dir/old.txt")
    ));
}

#[cfg(unix)]
#[test]
fn artifact_manifest_rejects_fifo_socket_and_hardlink_kinds() {
    use std::os::unix::net::UnixListener;

    let upper = TempDir::new().unwrap();
    let fifo = upper.path().join("pipe");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    unsafe {
        libc::mkfifo(fifo_c.as_ptr(), 0o600);
    }
    let _socket = UnixListener::bind(upper.path().join("sock")).unwrap();
    std::fs::write(upper.path().join("original"), "content").unwrap();
    std::fs::hard_link(upper.path().join("original"), upper.path().join("hardlink")).unwrap();

    let report = scan_overlay_upper(upper.path(), &empty_baseline()).unwrap();

    let rejected = report
        .rejected_kinds
        .iter()
        .map(|rejected| (rejected.path.as_str(), rejected.kind))
        .collect::<BTreeSet<_>>();
    assert!(rejected.contains(&("pipe", UnsupportedArtifactKind::Fifo)));
    assert!(rejected.contains(&("sock", UnsupportedArtifactKind::Socket)));
    assert!(rejected.contains(&("original", UnsupportedArtifactKind::Hardlink)));
    assert!(rejected.contains(&("hardlink", UnsupportedArtifactKind::Hardlink)));
}

#[cfg(unix)]
#[test]
fn artifact_manifest_classifies_device_without_privileged_fixture() {
    assert_eq!(
        classify_unix_special(false, false, true),
        Some(UnsupportedArtifactKind::Device)
    );
    assert_eq!(
        classify_unix_special(true, false, true),
        Some(UnsupportedArtifactKind::Fifo)
    );
    assert_eq!(
        classify_unix_special(false, true, true),
        Some(UnsupportedArtifactKind::Socket)
    );
}

#[cfg(unix)]
#[test]
fn artifact_manifest_records_normalized_mode() {
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("exec.sh"), "#!/bin/sh\n").unwrap();
    let mut perms = std::fs::metadata(upper.path().join("exec.sh"))
        .unwrap()
        .permissions();
    perms.set_mode(0o4755);
    std::fs::set_permissions(upper.path().join("exec.sh"), perms).unwrap();

    let report = scan_overlay_upper(upper.path(), &empty_baseline()).unwrap();

    let ArtifactEntry::Regular(entry) = &report.manifest.entries[0] else {
        panic!("expected regular entry");
    };
    assert_eq!(entry.mode, 0o755);
}

#[test]
fn artifact_adjudication_accepts_manifest_with_write_scope_and_effect_region() {
    let project = TempDir::new().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    let manifest = regular_manifest("src/generated.txt");
    let region = mutation_region(project.path(), "src");

    let report = adjudicate_artifact_manifest(
        &manifest,
        &[],
        project.path().to_str().unwrap(),
        &["src/**".to_string()],
        &region,
    )
    .unwrap();

    assert_eq!(
        report.artifact_manifest_status,
        ArtifactAdjudicationStatus::Accepted
    );
    assert_eq!(report.artifact_adjudication, "accepted");
    assert_eq!(report.observed_effects.len(), 1);
}

#[test]
fn artifact_adjudication_denies_unsupported_kind_before_scope_checks() {
    let project = TempDir::new().unwrap();
    let manifest = regular_manifest("../escape.txt");
    let rejected = vec![RejectedArtifactKind {
        path: "fifo".to_string(),
        kind: UnsupportedArtifactKind::Fifo,
        reason: "fifo artifact".to_string(),
    }];

    let failure = adjudicate_artifact_manifest(
        &manifest,
        &rejected,
        project.path().to_str().unwrap(),
        &[],
        &AuthorizedEffectRegion::empty(),
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::GuardDenied);
    assert_eq!(
        failure.report.artifact_manifest_status,
        ArtifactAdjudicationStatus::Denied
    );
    assert_eq!(
        failure.report.artifact_adjudication,
        "denied_unsupported_kind"
    );
    assert_eq!(
        failure.report.overlay_disposition,
        OverlayDisposition::Denied
    );
    assert_eq!(failure.report.rejected_kinds, rejected);
}

#[test]
fn artifact_adjudication_denies_path_traversal_and_absolute_escape_as_guard_denied() {
    let project = TempDir::new().unwrap();
    for path in ["../escape.txt", "/tmp/escape.txt"] {
        let failure = adjudicate_artifact_manifest(
            &regular_manifest(path),
            &[],
            project.path().to_str().unwrap(),
            &["src/**".to_string()],
            &AuthorizedEffectRegion::empty(),
        )
        .unwrap_err();

        assert_eq!(failure.error.code, CepErrorCode::GuardDenied);
        assert_eq!(failure.report.artifact_adjudication, "denied_path_escape");
    }
}

#[cfg(unix)]
#[test]
fn artifact_adjudication_denies_symlink_path_escape_as_guard_denied() {
    let project = TempDir::new().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    symlink("/tmp", project.path().join("src/link")).unwrap();
    let region = mutation_region(project.path(), "src");

    let failure = adjudicate_artifact_manifest(
        &regular_manifest("src/link"),
        &[],
        project.path().to_str().unwrap(),
        &["src/**".to_string()],
        &region,
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::GuardDenied);
    assert_eq!(failure.report.artifact_adjudication, "denied_path_escape");
}

#[test]
fn artifact_adjudication_preserves_guard_l1_write_path_exact_and_prefix_behavior() {
    let project = TempDir::new().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    let exact_region = AuthorizedEffectRegion {
        kinds: BTreeSet::from([ImpactKind::WorkspaceMutation]),
        write_paths: vec![project.path().join("src/exact.txt")],
        write_path_prefixes: Vec::new(),
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: false,
    };

    let exact = adjudicate_artifact_manifest(
        &regular_manifest("src/exact.txt"),
        &[],
        project.path().to_str().unwrap(),
        &["src/exact.txt".to_string()],
        &exact_region,
    );
    assert!(exact.is_ok());

    let prefix_region = mutation_region(project.path(), "src");
    let prefix = adjudicate_artifact_manifest(
        &regular_manifest("src/nested/generated.txt"),
        &[],
        project.path().to_str().unwrap(),
        &["src/**".to_string()],
        &prefix_region,
    );
    assert!(prefix.is_ok());
}

#[test]
fn artifact_adjudication_denies_write_path_miss_as_replan() {
    let project = TempDir::new().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    std::fs::create_dir(project.path().join("outside")).unwrap();
    let region = mutation_region(project.path(), "outside");

    let failure = adjudicate_artifact_manifest(
        &regular_manifest("outside/generated.txt"),
        &[],
        project.path().to_str().unwrap(),
        &["src/**".to_string()],
        &region,
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
    assert_eq!(failure.report.artifact_adjudication, "denied_write_scope");
    assert_eq!(failure.report.denied_paths, vec!["outside/generated.txt"]);
}

#[test]
fn artifact_adjudication_uses_effect_region_for_every_observed_effect() {
    let project = TempDir::new().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();

    let failure = adjudicate_artifact_manifest(
        &regular_manifest("src/generated.txt"),
        &[],
        project.path().to_str().unwrap(),
        &["src/**".to_string()],
        &AuthorizedEffectRegion::empty(),
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
    assert_eq!(failure.report.artifact_adjudication, "denied_effect_region");
    assert_eq!(failure.report.observed_effects.len(), 1);
    assert!(matches!(
        &failure.report.observed_effects[0].scope,
        ObservedScope::Path(path) if path.ends_with("src/generated.txt")
    ));
}

#[test]
fn artifact_prepare_commit_set_materializes_regular_bytes_and_rechecks_hashes() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::create_dir_all(upper.path().join("src")).unwrap();
    std::fs::write(upper.path().join("src/generated.txt"), "accepted").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "src/generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };

    let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();

    assert!(!prepared.staging_root().starts_with(project.path()));
    assert!(!prepared.staging_root().starts_with(upper.path()));
    let PreparedArtifactEntry::Regular(entry) = &prepared.entries[0] else {
        panic!("expected prepared regular entry");
    };
    assert_eq!(std::fs::read(&entry.staged_path).unwrap(), b"accepted");
    assert_eq!(entry.content_hash, hash_content(b"accepted"));
    assert_eq!(entry.mode, 0o644);
}

#[test]
fn artifact_prepare_commit_set_rejects_regular_hash_mismatch_before_commit() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "actual").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"manifest"),
            size_bytes: 6,
            mode: 0o644,
            lower_hash: None,
        })],
    };

    let error = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap_err();

    assert_eq!(error.code, CepErrorCode::ExecutionFailed);
    assert!(error.message.contains("hash mismatch"));
}

#[test]
fn artifact_commit_order_sorts_deletes_dirs_then_files_and_symlinks() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::create_dir_all(upper.path().join("a/nested")).unwrap();
    std::fs::create_dir_all(upper.path().join("b")).unwrap();
    std::fs::write(upper.path().join("a-file.txt"), "a").unwrap();
    std::fs::write(upper.path().join("z-file.txt"), "z").unwrap();
    #[cfg(unix)]
    symlink("target", upper.path().join("m-link")).unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let mut entries = vec![
        ArtifactEntry::Regular(RegularArtifactEntry {
            path: "z-file.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"z"),
            size_bytes: 1,
            mode: 0o644,
            lower_hash: None,
        }),
        ArtifactEntry::Directory(DirectoryArtifactEntry {
            path: "a/nested".to_string(),
            operation: ArtifactOperation::Create,
            mode: 0o755,
        }),
        ArtifactEntry::Delete(DeleteArtifactEntry {
            path: "old/deep/file.txt".to_string(),
            operation: ArtifactOperation::Delete,
            lower_kind: Some(ArtifactFileKind::RegularFile),
            lower_hash: Some("blake3:old".to_string()),
        }),
        ArtifactEntry::Directory(DirectoryArtifactEntry {
            path: "b".to_string(),
            operation: ArtifactOperation::Create,
            mode: 0o755,
        }),
        ArtifactEntry::Regular(RegularArtifactEntry {
            path: "a-file.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"a"),
            size_bytes: 1,
            mode: 0o644,
            lower_hash: None,
        }),
        ArtifactEntry::Delete(DeleteArtifactEntry {
            path: "old/file.txt".to_string(),
            operation: ArtifactOperation::Delete,
            lower_kind: Some(ArtifactFileKind::RegularFile),
            lower_hash: Some("blake3:old".to_string()),
        }),
    ];
    #[cfg(unix)]
    entries.push(ArtifactEntry::Symlink(SymlinkArtifactEntry {
        path: "m-link".to_string(),
        operation: ArtifactOperation::Create,
        target: "target".to_string(),
    }));

    let prepared =
        prepare_artifact_commit_set(ArtifactManifest { entries }, upper.path(), &baseline).unwrap();

    let paths = prepared
        .entries
        .iter()
        .map(|entry| (entry.apply_order().label(), entry.path().to_string()))
        .collect::<Vec<_>>();
    let mut expected = vec![
        ("delete_or_whiteout", "old/deep/file.txt".to_string()),
        ("delete_or_whiteout", "old/file.txt".to_string()),
        ("directory_create", "b".to_string()),
        ("directory_create", "a/nested".to_string()),
        ("create_or_update", "a-file.txt".to_string()),
    ];
    #[cfg(unix)]
    expected.push(("create_or_update", "m-link".to_string()));
    expected.push(("create_or_update", "z-file.txt".to_string()));
    assert_eq!(paths, expected);
}

#[test]
fn artifact_prepare_commit_set_drops_staged_content_on_denial_paths() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };

    let staging_root = {
        let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
        let root = prepared.staging_root().to_path_buf();
        assert!(root.exists());
        root
    };

    assert!(!staging_root.exists());
}

#[cfg(unix)]
#[test]
fn artifact_prepare_commit_set_uses_manifest_normalized_modes_and_rejects_xattrs() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("exec.sh"), "#!/bin/sh\n").unwrap();
    let mut perms = std::fs::metadata(upper.path().join("exec.sh"))
        .unwrap()
        .permissions();
    perms.set_mode(0o7755);
    std::fs::set_permissions(upper.path().join("exec.sh"), perms).unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let report = scan_overlay_upper(upper.path(), &baseline).unwrap();
    let prepared = prepare_artifact_commit_set(report.manifest, upper.path(), &baseline).unwrap();
    let PreparedArtifactEntry::Regular(entry) = &prepared.entries[0] else {
        panic!("expected prepared regular entry");
    };
    assert_eq!(entry.mode, 0o755);
    assert!(!format!("{entry:?}").contains("uid"));
    assert!(!format!("{entry:?}").contains("timestamp"));

    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let xattr_path = upper.path().join("xattr.txt");
        std::fs::write(&xattr_path, "x").unwrap();
        let path_c = CString::new(xattr_path.as_os_str().as_bytes()).unwrap();
        let name_c = CString::new("user.lattice_test").unwrap();
        let value = b"blocked";
        let rc = unsafe {
            libc::lsetxattr(
                path_c.as_ptr(),
                name_c.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        if rc == 0 {
            let report = scan_overlay_upper(upper.path(), &baseline).unwrap();
            assert!(report.rejected_kinds.iter().any(|rejected| {
                rejected.path == "xattr.txt" && rejected.kind == UnsupportedArtifactKind::Xattr
            }));
        }
    }
}

#[test]
fn artifact_commit_lower_drift_blocks_commit_before_workspace_mutation() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };
    let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
    std::fs::write(project.path().join("external.txt"), "drift").unwrap();

    let failure = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &successful_verify_cleanup(),
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
    assert_eq!(failure.report.commit_outcome, CommitOutcome::Denied);
    assert_eq!(failure.report.lower_consistency, "drift_detected");
    assert_eq!(failure.report.artifact_adjudication, "denied_lower_drift");
    assert_eq!(
        failure.report.overlay_disposition,
        OverlayDisposition::Denied
    );
    assert!(!project.path().join("generated.txt").exists());
    assert_eq!(failure.report.changed_paths, vec!["external.txt"]);
}

#[test]
fn commit_baseline_artifact_report_rejects_missing_anchor_before_lower_recheck() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("source.txt"), "alpha\nbeta\n").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };
    let evidence = vec![HunkAnchorEvidence {
        file_path: "source.txt".to_string(),
        hunk_index: 0,
        anchors: vec![anchor(1, "missing")],
    }];
    let prepared = prepare_artifact_commit_set_with_baseline_evidence(
        manifest,
        upper.path(),
        &baseline,
        evidence,
    )
    .unwrap();

    let failure = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &successful_verify_cleanup(),
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
    assert_eq!(failure.report.commit_outcome, CommitOutcome::Denied);
    assert_eq!(failure.report.lower_consistency, "not_checked");
    assert_eq!(failure.report.commit_baseline_validation, "failed");
    assert_eq!(
        failure.report.commit_baseline_failure_reason.as_deref(),
        Some("missing_anchor")
    );
    assert_eq!(
        failure.report.commit_baseline_details["hunks"][0]["match_result"],
        "missing_anchor"
    );
    assert!(!project.path().join("generated.txt").exists());
}

#[test]
fn commit_baseline_artifact_report_records_lower_drift_changed_paths() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };
    let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
    std::fs::write(project.path().join("external.txt"), "drift").unwrap();

    let failure = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &successful_verify_cleanup(),
    )
    .unwrap_err();

    assert_eq!(failure.report.commit_baseline_validation, "failed");
    assert_eq!(
        failure.report.commit_baseline_failure_reason.as_deref(),
        Some("lower_drift")
    );
    assert_eq!(
        failure.report.commit_baseline_details["changed_paths"],
        json!(["external.txt"])
    );
    assert_eq!(failure.report.lower_consistency, "drift_detected");
}

#[test]
fn commit_baseline_artifact_report_uses_captured_baseline_bytes_before_lower_recheck() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("source.txt"), "alpha\nbeta\n").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };
    let evidence = vec![HunkAnchorEvidence {
        file_path: "source.txt".to_string(),
        hunk_index: 0,
        anchors: vec![anchor(1, "alpha"), anchor(2, "beta")],
    }];
    let prepared = prepare_artifact_commit_set_with_baseline_evidence(
        manifest,
        upper.path(),
        &baseline,
        evidence,
    )
    .unwrap();
    std::fs::write(project.path().join("source.txt"), "shifted\nalpha\nbeta\n").unwrap();

    let failure = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &successful_verify_cleanup(),
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
    assert_eq!(failure.report.lower_consistency, "drift_detected");
    assert_eq!(
        failure.report.commit_baseline_failure_reason.as_deref(),
        Some("lower_drift")
    );
    assert_eq!(
        failure.report.commit_baseline_details["changed_paths"],
        json!(["source.txt"])
    );
    assert!(!project.path().join("generated.txt").exists());
}

#[test]
fn commit_baseline_artifact_report_rejects_verify_layer_contamination() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };
    let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
    let mut verify = successful_verify_cleanup();
    verify.contaminated_paths = vec!["verify-only.txt".to_string()];

    let failure = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &verify,
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
    assert_eq!(failure.report.lower_consistency, "not_checked");
    assert_eq!(failure.report.commit_baseline_validation, "failed");
    assert_eq!(
        failure.report.commit_baseline_failure_reason.as_deref(),
        Some("verify_layer_contamination")
    );
    assert_eq!(
        failure.report.commit_baseline_details["contaminated_paths"],
        json!(["verify-only.txt"])
    );
    assert!(!project.path().join("generated.txt").exists());
}

#[test]
fn commit_baseline_artifact_report_passes_after_anchor_and_lower_gates() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("source.txt"), "alpha\nbeta\n").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };
    let evidence = vec![HunkAnchorEvidence {
        file_path: "source.txt".to_string(),
        hunk_index: 0,
        anchors: vec![anchor(1, "alpha"), anchor(2, "beta")],
    }];
    let prepared = prepare_artifact_commit_set_with_baseline_evidence(
        manifest,
        upper.path(),
        &baseline,
        evidence,
    )
    .unwrap();

    let report = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &successful_verify_cleanup(),
    )
    .unwrap();

    assert_eq!(report.commit_outcome, CommitOutcome::Committed);
    assert_eq!(report.lower_consistency, "unchanged");
    assert_eq!(report.commit_baseline_validation, "passed");
    assert!(report.commit_baseline_failure_reason.is_none());
    assert_eq!(
        report.commit_baseline_details["hunks"][0]["match_result"],
        "matched"
    );
    assert_eq!(
        report.commit_baseline_details["hunks"][0]["anchor_count"],
        2
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("generated.txt")).unwrap(),
        "accepted"
    );
}

#[test]
fn artifact_commit_blocks_cleanup_failures_before_lower_recheck() {
    let project = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
            path: "generated.txt".to_string(),
            operation: ArtifactOperation::Create,
            content_hash: hash_content(b"accepted"),
            size_bytes: 8,
            mode: 0o644,
            lower_hash: None,
        })],
    };
    let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
    let mut cleanup = successful_artifact_cleanup();
    cleanup.outcome = CleanupOutcome::Failed;

    let failure = commit_artifact_manifest(
        project.path(),
        prepared,
        &cleanup,
        &successful_verify_cleanup(),
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ExecutionFailed);
    assert_eq!(failure.report.commit_outcome, CommitOutcome::NotStarted);
    assert_eq!(failure.report.lower_consistency, "not_checked");
    assert_eq!(
        failure.report.artifact_adjudication,
        "denied_cleanup_failed"
    );
    assert!(!project.path().join("generated.txt").exists());
}

#[cfg(unix)]
#[test]
fn artifact_commit_applies_successful_create_update_delete_directory_and_symlink() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("updated.txt"), "old").unwrap();
    std::fs::write(project.path().join("deleted.txt"), "delete").unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("created.txt"), "created").unwrap();
    std::fs::write(upper.path().join("updated.txt"), "new").unwrap();
    let manifest = ArtifactManifest {
        entries: vec![
            ArtifactEntry::Regular(RegularArtifactEntry {
                path: "created.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"created"),
                size_bytes: 7,
                mode: 0o644,
                lower_hash: None,
            }),
            ArtifactEntry::Regular(RegularArtifactEntry {
                path: "updated.txt".to_string(),
                operation: ArtifactOperation::Update,
                content_hash: hash_content(b"new"),
                size_bytes: 3,
                mode: 0o755,
                lower_hash: baseline.entries["updated.txt"].content_hash.clone(),
            }),
            ArtifactEntry::Delete(DeleteArtifactEntry {
                path: "deleted.txt".to_string(),
                operation: ArtifactOperation::Delete,
                lower_kind: Some(ArtifactFileKind::RegularFile),
                lower_hash: baseline.entries["deleted.txt"].content_hash.clone(),
            }),
            ArtifactEntry::Directory(DirectoryArtifactEntry {
                path: "new-dir/nested".to_string(),
                operation: ArtifactOperation::Create,
                mode: 0o755,
            }),
            ArtifactEntry::Symlink(SymlinkArtifactEntry {
                path: "new-link".to_string(),
                operation: ArtifactOperation::Create,
                target: "created.txt".to_string(),
            }),
        ],
    };
    let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();

    let report = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &successful_verify_cleanup(),
    )
    .unwrap();

    assert_eq!(report.commit_outcome, CommitOutcome::Committed);
    assert_eq!(report.lower_consistency, "unchanged");
    assert_eq!(
        std::fs::read_to_string(project.path().join("created.txt")).unwrap(),
        "created"
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("updated.txt")).unwrap(),
        "new"
    );
    assert!(!project.path().join("deleted.txt").exists());
    assert!(project.path().join("new-dir/nested").is_dir());
    assert_eq!(
        std::fs::read_link(project.path().join("new-link")).unwrap(),
        PathBuf::from("created.txt")
    );
    assert_eq!(report.applied_entries.len(), 5);
}

#[cfg(unix)]
#[test]
fn artifact_commit_delete_refuses_symlink_parent_and_preserves_outside_file() {
    let project = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::create_dir(project.path().join("allowed")).unwrap();
    std::fs::write(outside.path().join("victim.txt"), "outside").unwrap();
    symlink(outside.path(), project.path().join("allowed/link")).unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let upper = TempDir::new().unwrap();
    let manifest = ArtifactManifest {
        entries: vec![ArtifactEntry::Delete(DeleteArtifactEntry {
            path: "allowed/link/victim.txt".to_string(),
            operation: ArtifactOperation::Delete,
            lower_kind: Some(ArtifactFileKind::RegularFile),
            lower_hash: None,
        })],
    };
    let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();

    let failure = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &successful_verify_cleanup(),
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ExecutionFailed);
    assert!(
        failure
            .report
            .failed_entry
            .as_ref()
            .expect("failed entry")
            .error
            .contains("refusing to write through symlink parent")
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("victim.txt")).unwrap(),
        "outside"
    );
}

#[test]
fn artifact_commit_reports_partial_apply_failure_without_clearing_applied_entries() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("source.txt"), "alpha\nbeta\n").unwrap();
    std::fs::create_dir(project.path().join("z-blocked")).unwrap();
    let baseline = capture_lower_baseline(project.path()).unwrap();
    let upper = TempDir::new().unwrap();
    std::fs::write(upper.path().join("a-ok.txt"), "ok").unwrap();
    std::fs::write(upper.path().join("z-blocked"), "blocked").unwrap();
    let manifest = ArtifactManifest {
        entries: vec![
            ArtifactEntry::Regular(RegularArtifactEntry {
                path: "a-ok.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"ok"),
                size_bytes: 2,
                mode: 0o644,
                lower_hash: None,
            }),
            ArtifactEntry::Regular(RegularArtifactEntry {
                path: "z-blocked".to_string(),
                operation: ArtifactOperation::Update,
                content_hash: hash_content(b"blocked"),
                size_bytes: 7,
                mode: 0o644,
                lower_hash: None,
            }),
        ],
    };
    let evidence = vec![HunkAnchorEvidence {
        file_path: "source.txt".to_string(),
        hunk_index: 0,
        anchors: vec![anchor(1, "alpha"), anchor(2, "beta")],
    }];
    let prepared = prepare_artifact_commit_set_with_baseline_evidence(
        manifest,
        upper.path(),
        &baseline,
        evidence,
    )
    .unwrap();

    let failure = commit_artifact_manifest(
        project.path(),
        prepared,
        &successful_artifact_cleanup(),
        &successful_verify_cleanup(),
    )
    .unwrap_err();

    assert_eq!(failure.error.code, CepErrorCode::ExecutionFailed);
    assert_eq!(failure.report.commit_outcome, CommitOutcome::Partial);
    assert_eq!(
        failure.report.commit_baseline_details["hunks"][0]["match_result"],
        "matched"
    );
    assert_eq!(failure.report.applied_entries.len(), 1);
    assert_eq!(failure.report.applied_entries[0].path, "a-ok.txt");
    let failed = failure.report.failed_entry.expect("failed entry");
    assert_eq!(failed.path, "z-blocked");
    assert_eq!(failed.kind, ArtifactFileKind::RegularFile);
    assert!(!failed.error.is_empty());
    assert_eq!(
        std::fs::read_to_string(project.path().join("a-ok.txt")).unwrap(),
        "ok"
    );
}

#[test]
fn artifact_commit_serializes_concurrent_lower_recheck_and_apply_sections() {
    let observer_guard = artifact_commit_lock().lock().unwrap();
    TEST_COMMIT_SECTION_ACTIVE.store(0, Ordering::SeqCst);
    TEST_COMMIT_SECTION_MAX.store(0, Ordering::SeqCst);
    TEST_COMMIT_SECTION_DELAY.store(true, Ordering::SeqCst);
    drop(observer_guard);

    let cleanup = successful_artifact_cleanup();
    let verify = successful_verify_cleanup();
    let handles = (0..2)
        .map(|index| {
            let project = TempDir::new().unwrap();
            let upper = TempDir::new().unwrap();
            let filename = format!("generated-{index}.txt");
            std::fs::write(upper.path().join(&filename), "accepted").unwrap();
            let baseline = capture_lower_baseline(project.path()).unwrap();
            let manifest = ArtifactManifest {
                entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                    path: filename,
                    operation: ArtifactOperation::Create,
                    content_hash: hash_content(b"accepted"),
                    size_bytes: 8,
                    mode: 0o644,
                    lower_hash: None,
                })],
            };
            let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
            let cleanup = cleanup.clone();
            let verify = verify.clone();
            std::thread::spawn(move || {
                let result = commit_artifact_manifest(project.path(), prepared, &cleanup, &verify);
                assert!(result.is_ok());
                project
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        let _project = handle.join().unwrap();
    }
    let observer_guard = artifact_commit_lock().lock().unwrap();
    TEST_COMMIT_SECTION_DELAY.store(false, Ordering::SeqCst);

    assert_eq!(TEST_COMMIT_SECTION_MAX.load(Ordering::SeqCst), 1);
    drop(observer_guard);
}
