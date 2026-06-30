// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Equivalence verification.
//!
//! Gates migration success: the source is only archived after this
//! verifier returns `Ok(true)`. Compares topology (commit set, parent
//! edges under the idmap, branch ref tips) and transcripts (per-branch
//! message-by-message equivalence) between the SQLite source and the
//! Git target.
//!
//! Uses a tip-OID-direct reader (`read_transcript_from_tip`) that walks an
//! arbitrary commit OID without checking out branches or mutating `meta.json`
//! (recommendation, RESEARCH.md Open Question #5). This avoids the
//! `head_transcript` limitation — it only walks the HEAD ref and cannot
//! enumerate non-HEAD branches.

use std::collections::HashSet;

use crate::git_graph::helpers::{parse_messages_jsonl, read_tree_blob, session_ref_name};
use crate::graph::GraphMessage;
use crate::{Result, SessionError};

use super::extract::{Idmap, extract_all_with_conn};

/// Verify equivalence between the SQLite source and the Git target.
///
/// Returns `Ok(true)` if topology and transcripts match, `Ok(false)` on any
/// mismatch. The caller (`run_migration`) treats `false` as failure: the
/// source is NOT archived and the command exits non-zero.
///
/// Takes `conn` and `repo` directly so it is independently re-runnable via
/// `--verify-only` without re-running writes.
pub(crate) fn verify_equivalence(
    conn: &rusqlite::Connection,
    repo: &git2::Repository,
    idmap: &Idmap,
) -> Result<bool> {
    let sessions = extract_all_with_conn(conn)?;
    for session in &sessions {
        if !verify_session_topology(repo, session, idmap)? {
            return Ok(false);
        }
        if !verify_session_transcripts(repo, session, idmap)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Topology equivalence for one session.
fn verify_session_topology(
    repo: &git2::Repository,
    session: &super::extract::ExtractedSession,
    idmap: &Idmap,
) -> Result<bool> {
    // Branch ref tips: every SQLite ref must have a corresponding Git ref
    // pointing at the idmap-remapped commit OID.
    for r in &session.refs {
        let Some(legacy_cid) = r.commit_id.as_ref() else {
            continue;
        };
        let Some(expected_oid_str) = idmap.get(legacy_cid) else {
            // Unreachable tip — not migrated; skip (semantics).
            continue;
        };
        let expected_oid = git2::Oid::from_str(expected_oid_str)
            .map_err(|e| SessionError::Git(format!("parse expected ref oid: {e}")))?;
        let ref_name = session_ref_name(&session.session_id, &r.name)?;
        let git_ref = match repo.find_reference(&ref_name) {
            Ok(rf) => rf,
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                eprintln!(
                    "verify: session {} branch ref {} missing in Git target",
                    session.session_id, r.name
                );
                return Ok(false);
            }
            Err(e) => return Err(SessionError::Git(format!("find ref {ref_name}: {e}"))),
        };
        let actual_oid = git_ref
            .target()
            .ok_or_else(|| SessionError::Git(format!("ref {ref_name} has no target")))?;
        if actual_oid != expected_oid {
            eprintln!(
                "verify: session {} branch {} ref tip mismatch: expected {} got {}",
                session.session_id, r.name, expected_oid, actual_oid
            );
            return Ok(false);
        }
    }

    // Parent edge check: for each migrated commit, Git parent OIDs must equal
    // the idmap-remapped SQLite parent_ids.
    for commit in &session.commits {
        let Some(oid_str) = idmap.get(&commit.commit_id) else {
            eprintln!(
                "verify: commit {} not in idmap (topology gap)",
                commit.commit_id
            );
            return Ok(false);
        };
        let oid = git2::Oid::from_str(oid_str)
            .map_err(|e| SessionError::Git(format!("parse commit oid: {e}")))?;
        let git_commit = repo
            .find_commit(oid)
            .map_err(|e| SessionError::Git(format!("find commit {oid}: {e}")))?;

        let expected_parents: Vec<git2::Oid> = commit
            .parent_ids
            .iter()
            .map(|pid| {
                idmap
                    .get(pid)
                    .ok_or_else(|| {
                        SessionError::Git(format!(
                            "parent {pid} not in idmap for commit {}",
                            commit.commit_id
                        ))
                    })
                    .and_then(|s| {
                        git2::Oid::from_str(s)
                            .map_err(|e| SessionError::Git(format!("parse parent oid: {e}")))
                    })
            })
            .collect::<Result<Vec<_>>>()?;

        let actual_parents: Vec<git2::Oid> = git_commit.parent_ids().collect();

        if expected_parents != actual_parents {
            eprintln!(
                "verify: commit {} parent edge mismatch: expected {:?} got {:?}",
                commit.commit_id, expected_parents, actual_parents
            );
            return Ok(false);
        }
    }

    Ok(true)
}

/// Transcript equivalence for one session (per branch).
fn verify_session_transcripts(
    repo: &git2::Repository,
    session: &super::extract::ExtractedSession,
    idmap: &Idmap,
) -> Result<bool> {
    // Verify the transcript for each branch ref tip.
    for r in &session.refs {
        let Some(legacy_cid) = r.commit_id.as_ref() else {
            continue;
        };
        let Some(git_oid_str) = idmap.get(legacy_cid) else {
            continue;
        };
        let git_oid = git2::Oid::from_str(git_oid_str)
            .map_err(|e| SessionError::Git(format!("parse branch tip oid: {e}")))?;

        // SQLite transcript for this branch: walk the topologically-ordered
        // commits reachable from this tip and collect messages in order.
        let sqlite_msgs = collect_sqlite_transcript_from_tip(session, legacy_cid);

        // Git transcript via the tip-OID-direct reader.
        let git_msgs = read_transcript_from_tip(repo, git_oid)?;

        if sqlite_msgs.len() != git_msgs.len() {
            eprintln!(
                "verify: session {} branch {} transcript length mismatch: sqlite {} git {}",
                session.session_id,
                r.name,
                sqlite_msgs.len(),
                git_msgs.len()
            );
            return Ok(false);
        }

        for (i, (s, g)) in sqlite_msgs.iter().zip(git_msgs.iter()).enumerate() {
            if !messages_equal(s, g) {
                eprintln!(
                    "verify: session {} branch {} message {} mismatch: sqlite role={} content_len={} git role={} content_len={}",
                    session.session_id,
                    r.name,
                    i,
                    s.role,
                    s.content.len(),
                    g.role,
                    g.content.len()
                );
                return Ok(false);
            }
        }
    }

    // a detached-HEAD session (head_ref=NULL, head_commit_id=Some(tip))
    // with zero or non-covering graph_refs rows has its transcript reachable
    // only from head_commit_id. The ref loop above would compare nothing and
    // return Ok(true) — a vacuous pass before the source is archived. Seed
    // verification from the session HEAD when it is not already covered by a
    // verified ref tip.
    if let Some(head_legacy) = session.head_commit_id.as_ref() {
        let already_covered = session
            .refs
            .iter()
            .any(|r| r.commit_id.as_deref() == Some(head_legacy.as_str()));
        if !already_covered && let Some(git_oid_str) = idmap.get(head_legacy) {
            let git_oid = git2::Oid::from_str(git_oid_str)
                .map_err(|e| SessionError::Git(format!("parse head tip oid: {e}")))?;
            let sqlite_msgs = collect_sqlite_transcript_from_tip(session, head_legacy);
            let git_msgs = read_transcript_from_tip(repo, git_oid)?;
            if sqlite_msgs.len() != git_msgs.len() {
                eprintln!(
                    "verify: session {} detached-HEAD transcript length mismatch: sqlite {} git {}",
                    session.session_id,
                    sqlite_msgs.len(),
                    git_msgs.len()
                );
                return Ok(false);
            }
            for (i, (s, g)) in sqlite_msgs.iter().zip(git_msgs.iter()).enumerate() {
                if !messages_equal(s, g) {
                    eprintln!(
                        "verify: session {} detached-HEAD message {} mismatch: sqlite role={} content_len={} git role={} content_len={}",
                        session.session_id,
                        i,
                        s.role,
                        s.content.len(),
                        g.role,
                        g.content.len()
                    );
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

/// Collect the SQLite transcript reachable from a given tip commit, in
/// topological order (parents before children). Reuses the extractor's
/// pre-computed topological order — the ExtractedSession.commits vector is
/// already parents-before-children, so we filter to the reachable set.
fn collect_sqlite_transcript_from_tip(
    session: &super::extract::ExtractedSession,
    tip_legacy_id: &str,
) -> Vec<GraphMessage> {
    // Build the reachable set from the tip by walking parent_ids.
    let mut reachable: HashSet<&str> = HashSet::new();
    let mut stack = vec![tip_legacy_id];
    let by_id: std::collections::HashMap<&str, &super::extract::ExtractedCommit> = session
        .commits
        .iter()
        .map(|c| (c.commit_id.as_str(), c))
        .collect();
    while let Some(cid) = stack.pop() {
        if !reachable.insert(cid) {
            continue;
        }
        if let Some(commit) = by_id.get(cid) {
            for pid in &commit.parent_ids {
                stack.push(pid.as_str());
            }
        }
    }

    // Iterate in the pre-computed topological order and collect messages.
    let mut msgs = Vec::new();
    for commit in &session.commits {
        if reachable.contains(commit.commit_id.as_str()) {
            msgs.extend(commit.messages.iter().cloned());
        }
    }
    msgs
}

/// Tip-OID-direct reader (RESEARCH.md Open Question #5).
///
/// Walks the commit DAG from an arbitrary tip OID (TOPOLOGICAL|REVERSE
/// revwalk, same as `head_transcript_impl` but starting from a given OID, not
/// from meta.json HEAD). Reads `turn/messages.jsonl` from each commit tree.
/// Does NOT check out branches or mutate `meta.json`.
fn read_transcript_from_tip(
    repo: &git2::Repository,
    tip_oid: git2::Oid,
) -> Result<Vec<GraphMessage>> {
    let mut revwalk = repo
        .revwalk()
        .map_err(|e| SessionError::Git(format!("create revwalk: {e}")))?;
    revwalk
        .push(tip_oid)
        .map_err(|e| SessionError::Git(format!("push revwalk: {e}")))?;
    revwalk
        .set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)
        .map_err(|e| SessionError::Git(format!("set revwalk sorting: {e}")))?;

    let mut all_messages = Vec::new();
    for oid in revwalk {
        let oid = oid.map_err(|e| SessionError::Git(format!("revwalk error: {e}")))?;
        let commit = repo
            .find_commit(oid)
            .map_err(|e| SessionError::Git(format!("find commit: {e}")))?;
        let bytes = read_tree_blob(repo, &commit, "turn/messages.jsonl")?;
        let jsonl = String::from_utf8(bytes)
            .map_err(|e| SessionError::Git(format!("messages.jsonl not utf8: {e}")))?;
        let messages = parse_messages_jsonl(&jsonl)?;
        all_messages.extend(messages);
    }
    Ok(all_messages)
}

/// Field-by-field message comparison for clearer error reporting on mismatch.
/// GraphMessage derives PartialEq, but comparing field-by-field lets us log
/// which field differs.
fn messages_equal(a: &GraphMessage, b: &GraphMessage) -> bool {
    a.role == b.role
        && a.content == b.content
        && a.reasoning_content == b.reasoning_content
        && a.tool_calls == b.tool_calls
        && a.tool_call_id == b.tool_call_id
        && a.name == b.name
}
