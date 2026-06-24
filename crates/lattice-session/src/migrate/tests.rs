// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Migration test suite: topology, transcript, restartability,
//! no-SQLite-in-production, native OID stability.
//!
//! Fixtures build SQLite session DBs via direct SQL INSERTs into the five
//! source tables (sqlite_graph/ is deleted; RESEARCH.md is the DDL reference).

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::git_graph::helpers::{parse_manifest, read_tree_blob};
use crate::git_graph::GitSessionGraphStore;
use crate::graph::{GraphMessage, SessionGraphStore, TurnCommitInput, TurnCommitStatus, WorkspaceDisposition};
use crate::migrate::{MigrateOptions, run_migration};

// ─── DDL (from RESEARCH.md §Source Read) ───────────────────────────────────

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS graph_sessions (
    session_id TEXT PRIMARY KEY,
    label TEXT,
    active_agent_name TEXT NOT NULL,
    head_ref TEXT,
    head_commit_id TEXT,
    project_target_branch TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS graph_refs (
    session_id TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    commit_id TEXT,
    active_agent_name TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (session_id, kind, name)
);
CREATE TABLE IF NOT EXISTS graph_commits (
    commit_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    parents_json TEXT NOT NULL,
    message_ids_json TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    profile_snapshot_hash TEXT NOT NULL,
    profile_snapshot_json TEXT NOT NULL,
    model TEXT NOT NULL,
    provider TEXT NOT NULL,
    status TEXT NOT NULL,
    commit_message TEXT NOT NULL,
    usage_json TEXT,
    terminal_reason TEXT,
    disposition TEXT NOT NULL DEFAULT 'not_applicable',
    lifecycle_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS graph_messages (
    message_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    reasoning_content TEXT,
    tool_call_id TEXT,
    name TEXT,
    tool_calls_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    ordinal INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS graph_snapshot_state (
    commit_id TEXT PRIMARY KEY,
    snapshot_lost INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);
"#;

// ─── Fixture helpers ───────────────────────────────────────────────────────

fn seed_sqlite_db(dir: &Path, name: &str) -> PathBuf {
    let db_path = dir.join(name);
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute_batch(DDL).expect("create tables");
    db_path
}

fn insert_session(
    conn: &Connection,
    session_id: &str,
    label: Option<&str>,
    head_ref: Option<&str>,
    head_commit_id: Option<&str>,
    project_target_branch: &str,
) {
    conn.execute(
        "INSERT INTO graph_sessions (session_id, label, active_agent_name, head_ref, head_commit_id, project_target_branch, created_at, updated_at)
         VALUES (?1, ?2, 'test', ?3, ?4, ?5, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![session_id, label, head_ref, head_commit_id, project_target_branch],
    )
    .expect("insert session");
}

fn insert_ref(conn: &Connection, session_id: &str, name: &str, commit_id: &str) {
    conn.execute(
        "INSERT INTO graph_refs (session_id, name, kind, commit_id, active_agent_name, updated_at)
         VALUES (?1, ?2, 'branch', ?3, 'test', '2026-01-01T00:00:00Z')",
        rusqlite::params![session_id, name, commit_id],
    )
    .expect("insert ref");
}

fn insert_commit(
    conn: &Connection,
    commit_id: &str,
    session_id: &str,
    parents: &[&str],
    message_ids: &[&str],
    created_at: &str,
) {
    let parents_json = serde_json::to_string(parents).unwrap();
    let message_ids_json = serde_json::to_string(message_ids).unwrap();
    conn.execute(
        "INSERT INTO graph_commits
         (commit_id, session_id, parents_json, message_ids_json, agent_name,
          profile_snapshot_hash, profile_snapshot_json, model, provider,
          status, commit_message, usage_json, terminal_reason, disposition,
          lifecycle_json, created_at)
         VALUES (?1, ?2, ?3, ?4, 'test', 'test-hash', '{}', 'test-model',
                 'test-provider', 'completed', ?5, NULL, NULL, 'not_applicable',
                 '{}', ?6)",
        rusqlite::params![commit_id, session_id, parents_json, message_ids_json,
                          format!("Turn {commit_id}"), created_at],
    )
    .expect("insert commit");
}

fn insert_message(
    conn: &Connection,
    message_id: &str,
    session_id: &str,
    role: &str,
    content: &str,
    ordinal: i64,
) {
    conn.execute(
        "INSERT INTO graph_messages
         (message_id, session_id, role, content, reasoning_content, tool_call_id,
          name, tool_calls_json, created_at, ordinal)
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, '[]', '2026-01-01T00:00:00Z', ?5)",
        rusqlite::params![message_id, session_id, role, content, ordinal],
    )
    .expect("insert message");
}

/// Seed a linear session with N commits, each having a user+assistant message pair.
fn seed_linear(conn: &Connection, session_id: &str, n: usize) -> Vec<String> {
    insert_session(conn, session_id, Some("Linear test"), Some("main"), None, "main");
    let mut commit_ids: Vec<String> = Vec::new();
    for i in 0..n {
        let cid = format!("{session_id}-c{}", i + 1);
        let mid_user = format!("{cid}-m-user");
        let mid_asst = format!("{cid}-m-asst");
        let parent_refs: Vec<&str> = if i == 0 {
            vec![]
        } else {
            vec![commit_ids[i - 1].as_str()]
        };
        insert_commit(
            conn,
            &cid,
            session_id,
            &parent_refs,
            &[&mid_user, &mid_asst],
            &format!("2026-01-01T00:{:02}:00Z", i),
        );
        insert_message(conn, &mid_user, session_id, "user", &format!("prompt {}", i + 1), 0);
        insert_message(conn, &mid_asst, session_id, "assistant", &format!("response {}", i + 1), 1);
        commit_ids.push(cid);
    }
    let head = commit_ids.last().unwrap();
    insert_ref(conn, session_id, "main", head);
    conn.execute(
        "UPDATE graph_sessions SET head_commit_id = ?1 WHERE session_id = ?2",
        rusqlite::params![head, session_id],
    )
    .expect("update head");
    commit_ids
}

/// Seed a branched session: main (b1→b2) + feature (b1→b3).
fn seed_branched(conn: &Connection, session_id: &str) -> Vec<String> {
    insert_session(conn, session_id, Some("Branched test"), Some("main"), None, "main");
    let b1 = format!("{session_id}-b1");
    let b2 = format!("{session_id}-b2");
    let b3 = format!("{session_id}-b3");
    for (cid, parents, ts) in [
        (&b1, Vec::<&str>::new(), "00"),
        (&b2, vec![&b1], "01"),
        (&b3, vec![&b1], "02"),
    ] {
        let mid_u = format!("{cid}-mu");
        let mid_a = format!("{cid}-ma");
        let parent_refs: Vec<&str> = parents.clone();
        insert_commit(conn, cid, session_id, &parent_refs, &[&mid_u, &mid_a], &format!("2026-01-01T00:{ts}:00Z"));
        insert_message(conn, &mid_u, session_id, "user", &format!("prompt {cid}"), 0);
        insert_message(conn, &mid_a, session_id, "assistant", &format!("response {cid}"), 1);
    }
    insert_ref(conn, session_id, "main", &b2);
    insert_ref(conn, session_id, "feature", &b3);
    conn.execute(
        "UPDATE graph_sessions SET head_commit_id = ?1 WHERE session_id = ?2",
        rusqlite::params![&b2, session_id],
    )
    .expect("update head");
    vec![b1, b2, b3]
}

/// Seed a reset session: 3 commits on main, then reset main ref to c1.
/// c2 and c3 become unreachable.
fn seed_reset(conn: &Connection, session_id: &str) -> Vec<String> {
    insert_session(conn, session_id, Some("Reset test"), Some("main"), None, "main");
    let c1 = format!("{session_id}-c1");
    let c2 = format!("{session_id}-c2");
    let c3 = format!("{session_id}-c3");
    for (cid, parents, ts) in [
        (&c1, Vec::<&str>::new(), "00"),
        (&c2, vec![&c1], "01"),
        (&c3, vec![&c2], "02"),
    ] {
        let mid_u = format!("{cid}-mu");
        let mid_a = format!("{cid}-ma");
        let parent_refs: Vec<&str> = parents.clone();
        insert_commit(conn, cid, session_id, &parent_refs, &[&mid_u, &mid_a], &format!("2026-01-01T00:{ts}:00Z"));
        insert_message(conn, &mid_u, session_id, "user", &format!("prompt {cid}"), 0);
        insert_message(conn, &mid_a, session_id, "assistant", &format!("response {cid}"), 1);
    }
    // Reset: main ref points to c1 (not c3). c2/c3 unreachable.
    insert_ref(conn, session_id, "main", &c1);
    conn.execute(
        "UPDATE graph_sessions SET head_commit_id = ?1 WHERE session_id = ?2",
        rusqlite::params![&c1, session_id],
    )
    .expect("update head");
    vec![c1, c2, c3]
}

fn migrate_opts(source_db: PathBuf, target_repo: PathBuf) -> MigrateOptions {
    MigrateOptions {
        source_db,
        target_repo,
        dry_run: false,
        verify_only: false,
        purge_source: false,
    }
}

/// Count commits reachable from a ref tip via revwalk.
fn count_commits_from_ref(repo: &git2::Repository, ref_name: &str) -> usize {
    let rf = repo.find_reference(ref_name).expect("find ref");
    let tip = rf.target().expect("ref target");
    let mut walk = repo.revwalk().expect("revwalk");
    walk.push(tip).expect("push");
    walk.count()
}

// ─── Topology equivalence ─────────────────────────────────────────────

#[test]
fn topology_equivalence_linear() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seed_sqlite_db(dir.path(), "sessions.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_linear(&conn, "A", 3);
    drop(conn);

    let target = dir.path().join("sessions.git");
    let result = run_migration(&migrate_opts(db_path, target.clone())).expect("migration");
    assert!(result.verification_passed, "verification should pass");
    assert_eq!(result.commits_migrated, 3);

    let repo = git2::Repository::open(&target).unwrap();
    let ref_name = "refs/lattice/sessions/A/main";
    assert_eq!(count_commits_from_ref(&repo, ref_name), 3, "should have 3 commits");
}

#[test]
fn topology_equivalence_branched() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seed_sqlite_db(dir.path(), "sessions.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_branched(&conn, "B");
    drop(conn);

    let target = dir.path().join("sessions.git");
    let result = run_migration(&migrate_opts(db_path, target.clone())).expect("migration");
    assert!(result.verification_passed);

    let repo = git2::Repository::open(&target).unwrap();
    let main_ref = "refs/lattice/sessions/B/main";
    let feature_ref = "refs/lattice/sessions/B/feature";
    assert_eq!(count_commits_from_ref(&repo, main_ref), 2, "main should have 2 commits");
    assert_eq!(count_commits_from_ref(&repo, feature_ref), 2, "feature should have 2 commits (b1+b3)");
}

#[test]
fn topology_equivalence_reset() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seed_sqlite_db(dir.path(), "sessions.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_reset(&conn, "C");
    drop(conn);

    let target = dir.path().join("sessions.git");
    let result = run_migration(&migrate_opts(db_path, target.clone())).expect("migration");
    assert!(result.verification_passed);

    let repo = git2::Repository::open(&target).unwrap();
    let main_ref = "refs/lattice/sessions/C/main";
    // only c1 is reachable after reset; c2/c3 are orphans, not migrated.
    assert_eq!(count_commits_from_ref(&repo, main_ref), 1, "reset should leave only 1 reachable commit");
}

// ─── Transcript equivalence ───────────────────────────────────────────

#[test]
fn transcript_equivalence_linear() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seed_sqlite_db(dir.path(), "sessions.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_linear(&conn, "A", 3);
    drop(conn);

    let target = dir.path().join("sessions.git");
    let result = run_migration(&migrate_opts(db_path, target.clone())).expect("migration");
    assert!(result.verification_passed);

    let repo = git2::Repository::open(&target).unwrap();
    let ref_name = "refs/lattice/sessions/A/main";
    let rf = repo.find_reference(ref_name).expect("find ref");
    let tip = rf.target().expect("ref target");

    // Walk the Git DAG and collect messages (topological order).
    let mut walk = repo.revwalk().expect("revwalk");
    walk.push(tip).expect("push");
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE).expect("sort");

    let mut all_messages = Vec::new();
    for oid in walk {
        let oid = oid.expect("walk oid");
        let commit = repo.find_commit(oid).expect("find commit");
        let bytes = read_tree_blob(&repo, &commit, "turn/messages.jsonl").expect("read messages");
        let jsonl = String::from_utf8(bytes).expect("utf8");
        let msgs = crate::git_graph::helpers::parse_messages_jsonl(&jsonl).expect("parse");
        all_messages.extend(msgs);
    }

    // 3 commits × 2 messages = 6 total.
    assert_eq!(all_messages.len(), 6, "should have 6 messages total");
    // Check ordering: user/assistant pairs in commit order.
    for i in 0..3 {
        let u = &all_messages[i * 2];
        let a = &all_messages[i * 2 + 1];
        assert_eq!(u.role, "user", "message {} role", i * 2);
        assert_eq!(u.content, format!("prompt {}", i + 1), "message {} content", i * 2);
        assert_eq!(a.role, "assistant", "message {} role", i * 2 + 1);
        assert_eq!(a.content, format!("response {}", i + 1), "message {} content", i * 2 + 1);
    }
}

// ─── Restartability ───────────────────────────────────────────────────

#[test]
fn restart_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seed_sqlite_db(dir.path(), "sessions.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_linear(&conn, "R", 5);
    drop(conn);

    let target = dir.path().join("sessions.git");

    // First run.
    let result1 = run_migration(&migrate_opts(db_path.clone(), target.clone())).expect("migration 1");
    assert!(result1.verification_passed);
    assert_eq!(result1.commits_migrated, 5);

    let repo = git2::Repository::open(&target).unwrap();
    let ref_name = "refs/lattice/sessions/R/main";
    assert_eq!(count_commits_from_ref(&repo, ref_name), 5, "first run: 5 commits");

    // Second run (restart) — source was archived by first run, so we need to
    // re-seed for the restart test. Actually, the first run archives the source.
    // For restart testing, use purge_source=false (default archive). The archived
    // source is at sessions.db.migrated-<ts>. For a true restart test, we need
    // the source to still be at the original path. So let's re-seed.
    let db_path2 = seed_sqlite_db(dir.path(), "sessions.db");
    let conn2 = Connection::open(&db_path2).unwrap();
    seed_linear(&conn2, "R", 5);
    drop(conn2);

    // Second run with the re-seeded source.
    let result2 = run_migration(&migrate_opts(db_path2, target.clone())).expect("migration 2");
    assert!(result2.verification_passed);
    assert_eq!(result2.commits_migrated, 5, "restart should not duplicate");

    let repo2 = git2::Repository::open(&target).unwrap();
    assert_eq!(count_commits_from_ref(&repo2, ref_name), 5, "restart: still 5 commits, no duplicates");
}

/// Count every commit object in the repository's object database, regardless
/// of reachability. A duplicate-write bug would inflate this beyond the
/// reachable set.
fn count_all_commit_objects(repo: &git2::Repository) -> usize {
    let odb = repo.odb().expect("odb");
    let mut count = 0usize;
    odb.foreach(|oid| {
        if let Ok((_len, kind)) = odb.read_header(*oid) {
            if kind == git2::ObjectType::Commit {
                count += 1;
            }
        }
        true
    })
    .expect("odb foreach");
    count
}

/// simulate an *interrupted* run by writing only a subset of a
/// session's commits (with a fresh idmap that records just those), then run the
/// full migration against the same source. The completed run must reach all 5
/// commits with no duplicate commit objects — exercising the self-healing
/// skip and the content-addressed restart guarantee, which `restart_idempotent`
/// (two full runs) does not cover.
#[test]
fn restart_recovers_from_partial_write() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seed_sqlite_db(dir.path(), "sessions.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_linear(&conn, "P", 5);

    let target = dir.path().join("sessions.git");
    let repo = crate::migrate::open_or_init_target(&target).expect("init target");

    // Partial write: extract, truncate to the first 2 commits (topological
    // order guarantees these are the earliest), and write them with a fresh
    // idmap. This mimics a crash after 2 objects were written.
    let sessions = crate::migrate::extract::extract_all_with_conn(&conn).expect("extract");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].commits.len(), 5);
    let mut partial = sessions[0].clone();
    partial.commits.truncate(2);
    // Drop refs/head so the partial write does not create the final ref tip —
    // only the 2 commit objects land, like a crash mid-Step-1.
    partial.refs.clear();
    partial.head_commit_id = None;
    partial.head_ref = None;

    let mut idmap = crate::migrate::extract::Idmap::default();
    crate::migrate::write::write_migration(&repo, std::slice::from_ref(&partial), &mut idmap, &target)
        .expect("partial write");
    idmap.save(&target).expect("save partial idmap");
    assert_eq!(count_all_commit_objects(&repo), 2, "partial run wrote 2 commit objects");

    drop(conn);

    // Full run against the same source. Must converge to 5 reachable commits
    // with no duplicate objects (the 2 pre-written commits are reused).
    let result = run_migration(&migrate_opts(db_path, target.clone())).expect("full migration");
    assert!(result.verification_passed, "verification should pass after recovery");
    assert_eq!(result.commits_migrated, 5);

    let repo2 = git2::Repository::open(&target).unwrap();
    let ref_name = "refs/lattice/sessions/P/main";
    assert_eq!(
        count_commits_from_ref(&repo2, ref_name),
        5,
        "recovery: ref reaches all 5 commits"
    );
    assert_eq!(
        count_all_commit_objects(&repo2),
        5,
        "recovery: exactly 5 commit objects, no duplicates from the partial write"
    );
}

// ─── Native OID stability ────────────────────────────────────────────

#[test]
fn native_manifest_oid_stable() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("sessions.git");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let store = GitSessionGraphStore::new(&repo_dir);
    store
        .create_session("native-test", Some("Native OID test"), "coder", "main")
        .expect("create session");

    let messages = vec![
        GraphMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        GraphMessage {
            role: "assistant".to_string(),
            content: "hi".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    ];
    let input = TurnCommitInput {
        session_id: "native-test".to_string(),
        parent_ids: vec![],
        messages,
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: "Native test turn".to_string(),
        usage: None,
        terminal_reason: None,
    };
    let turn_commit = store.commit_turn(input).expect("commit turn");

    // Read the manifest from the Git tree.
    let repo = git2::Repository::open(&repo_dir).expect("open repo");
    let oid = git2::Oid::from_str(&turn_commit.commit_id).expect("parse oid");
    let commit = repo.find_commit(oid).expect("find commit");
    let manifest_bytes = read_tree_blob(&repo, &commit, "turn/manifest.json").expect("read manifest");
    let manifest = parse_manifest(&manifest_bytes).expect("parse manifest");

    // native commits have no legacy fields.
    assert!(manifest.legacy_session_id.is_none(), "native commit should have no legacy_session_id");
    assert!(manifest.legacy_commit_id.is_none(), "native commit should have no legacy_commit_id");

    // The raw JSON should NOT contain the legacy keys (skip_serializing_if = Option::is_none).
    let raw_json = String::from_utf8(manifest_bytes).expect("utf8");
    assert!(
        !raw_json.contains("legacy_session_id"),
        "native manifest JSON should not contain legacy_session_id key"
    );
    assert!(
        !raw_json.contains("legacy_commit_id"),
        "native manifest JSON should not contain legacy_commit_id key"
    );
}

// ─── No SQLite write path in production ───────────────────────────────

#[test]
fn no_sqlite_write_path_in_production() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    // lattice-api/src/session.rs: no pub fn sqlite, no SqliteSessionGraphStore.
    let session_rs = std::fs::read_to_string(workspace_root.join("crates/lattice-api/src/session.rs"))
        .expect("read session.rs");
    let sqlite_fn = format!("pub fn {}", "sqlite");
    let sqlite_store = format!("{}GraphStore", "SqliteSession");
    assert!(
        !session_rs.contains(&sqlite_fn),
        "SessionApi::sqlite should be deleted from session.rs"
    );
    assert!(
        !session_rs.contains(&sqlite_store),
        "SqliteSessionGraphStore should be deleted from session.rs"
    );

    // lattice-session/src/lib.rs: no pub mod sqlite_graph, no SqliteSessionGraphStore.
    let lib_rs = std::fs::read_to_string(workspace_root.join("crates/lattice-session/src/lib.rs"))
        .expect("read lib.rs");
    let sqlite_mod = format!("pub mod {}", "sqlite_graph");
    assert!(
        !lib_rs.contains(&sqlite_mod),
        "pub mod sqlite_graph should be deleted from lib.rs"
    );
    assert!(
        !lib_rs.contains(&sqlite_store),
        "SqliteSessionGraphStore should be deleted from lib.rs"
    );

    // sqlite_graph/ directory should not exist.
    assert!(
        !workspace_root.join("crates/lattice-session/src/sqlite_graph").exists(),
        "sqlite_graph/ directory should be deleted"
    );
}

// ─── legacy DB filename probing ─────────────────────────────────────────

#[test]
fn find_legacy_session_db_probes_session_v12_db_first() {
    let dir = tempfile::tempdir().unwrap();

    // No legacy DB → None.
    assert!(crate::migrate::find_legacy_session_db(dir.path()).is_none());

    // session_v12.db alone → found.
    let v12 = dir.path().join("session_v12.db");
    std::fs::write(&v12, b"").unwrap();
    assert_eq!(
        crate::migrate::find_legacy_session_db(dir.path()),
        Some(v12.clone())
    );

    // Add sessions.db (plural) — session_v12.db still wins (priority).
    let plural = dir.path().join("sessions.db");
    std::fs::write(&plural, b"").unwrap();
    assert_eq!(
        crate::migrate::find_legacy_session_db(dir.path()),
        Some(v12)
    );

    // Fresh dir with only sessions.db → found (fallback works).
    let dir2 = tempfile::tempdir().unwrap();
    let plural2 = dir2.path().join("sessions.db");
    std::fs::write(&plural2, b"").unwrap();
    assert_eq!(
        crate::migrate::find_legacy_session_db(dir2.path()),
        Some(plural2)
    );
}

#[test]
fn migration_works_with_session_v12_db_filename() {
    let dir = tempfile::tempdir().unwrap();
    // Use the REAL legacy filename, not the plural sessions.db.
    let db_path = seed_sqlite_db(dir.path(), "session_v12.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_linear(&conn, "V12", 3);
    drop(conn);

    // Verify the helper discovers it.
    assert_eq!(
        crate::migrate::find_legacy_session_db(dir.path()),
        Some(db_path.clone())
    );

    let target = dir.path().join("sessions.git");
    let result = run_migration(&migrate_opts(db_path, target.clone())).expect("migration");
    assert!(result.verification_passed, "verification should pass for session_v12.db");
    assert_eq!(result.commits_migrated, 3);

    let repo = git2::Repository::open(&target).unwrap();
    let ref_name = "refs/lattice/sessions/V12/main";
    assert_eq!(count_commits_from_ref(&repo, ref_name), 3, "session_v12.db migration: 3 commits");

    // The archive name should derive from the actual filename, not sessions.db.
    assert!(
        result.source_archived.as_ref().is_some(),
        "source should be archived"
    );
    let archived = result.source_archived.unwrap();
    assert!(
        archived.to_string_lossy().contains("session_v12.db.migrated-"),
        "archive name should derive from session_v12.db, got: {}",
        archived.display()
    );
}

// ─── dry-run performs NO target writes ──────────────────────────────────

#[test]
fn dry_run_creates_no_target_skeleton() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seed_sqlite_db(dir.path(), "session_v12.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_linear(&conn, "DRY", 2);
    drop(conn);

    let target = dir.path().join("sessions.git");

    // Pre-condition: target does not exist.
    assert!(!target.exists(), "target should not exist before dry-run");

    let opts = MigrateOptions {
        source_db: db_path.clone(),
        target_repo: target.clone(),
        dry_run: true,
        verify_only: false,
        purge_source: false,
    };
    let result = run_migration(&opts).expect("dry-run");

    // dry-run reports the plan but performs NO writes.
    assert_eq!(result.sessions_migrated, 1);
    assert_eq!(result.commits_migrated, 2);
    assert!(!result.verification_passed, "dry-run should not verify");
    assert!(result.source_archived.is_none(), "dry-run should not archive");

    // GAP 2 core assertion: the target directory/repo was NOT created.
    assert!(
        !target.exists(),
        "dry-run must NOT create the target repo skeleton (no-writes contract)"
    );

    // Source is untouched.
    assert!(db_path.exists(), "dry-run must not touch the source DB");
}

#[test]
fn verify_only_errors_when_target_absent() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seed_sqlite_db(dir.path(), "session_v12.db");
    let conn = Connection::open(&db_path).unwrap();
    seed_linear(&conn, "VO", 2);
    drop(conn);

    let target = dir.path().join("sessions.git");
    assert!(!target.exists(), "target should not exist");

    let opts = MigrateOptions {
        source_db: db_path,
        target_repo: target.clone(),
        dry_run: false,
        verify_only: true,
        purge_source: false,
    };
    let result = run_migration(&opts);

    // verify-only must error when the target is absent (no auto-init).
    assert!(
        result.is_err(),
        "verify-only should error when target does not exist (no auto-init side effect)"
    );
    // And must NOT have created the target.
    assert!(
        !target.exists(),
        "verify-only must not create the target repo when it errors"
    );
}
