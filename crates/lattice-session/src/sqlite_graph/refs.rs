//! Branch ref operations: checkout, branch, reset, diff.

use chrono::Utc;
use rusqlite::{params, OptionalExtension};

use crate::graph::{GraphDiff, GraphHead, GraphRef, GraphRefKind, ResetResult};
use crate::{Result, SessionError};

use super::SqliteSessionGraphStore;

impl SqliteSessionGraphStore {
    pub(super) fn checkout_impl(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        // Verify session exists.
        let _: String = conn
            .query_row(
                "SELECT session_id FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|_| SessionError::NotFound(session_id.to_string()))?;

        // Try target as a branch name first.
        // Distinguish "branch exists with NULL commit_id" from "branch missing".
        let branch_row: Option<Option<String>> = conn
            .query_row(
                "SELECT commit_id FROM graph_refs WHERE session_id=?1 AND kind='branch' AND name=?2",
                params![session_id, target],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        if let Some(commit_id) = branch_row {
            // Branch exists (commit_id may be NULL).
            conn.execute(
                "UPDATE graph_sessions SET head_ref=?1, head_commit_id=?2, updated_at=?3 WHERE session_id=?4",
                params![target, commit_id, now, session_id],
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
            return Ok(GraphHead::Branch {
                name: target.to_string(),
            });
        }

        // Try target as a commit ID.
        let commit_exists: bool =
            conn.query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                params![target, session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })? > 0;

        if commit_exists {
            // Detach HEAD at the commit.
            conn.execute(
                "UPDATE graph_sessions SET head_ref=NULL, head_commit_id=?1, updated_at=?2 WHERE session_id=?3",
                params![target, now, session_id],
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
            return Ok(GraphHead::Detached {
                commit_id: target.to_string(),
            });
        }

        // Target is neither a branch nor a commit in this session.
        Err(SessionError::MissingCommit(target.to_string()))
    }

    pub(super) fn branch_impl(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
    ) -> Result<GraphRef> {
        crate::validate_branch_name(name)?;
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        // Resolve the target commit ID.
        let commit_id = match from {
            Some(cid) => {
                // Verify the commit exists in this session.
                let exists: bool =
                    conn.query_row(
                        "SELECT COUNT(*) FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                        params![cid, session_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })? > 0;
                if !exists {
                    return Err(SessionError::MissingCommit(cid.to_string()));
                }
                cid.to_string()
            }
            None => {
                // Use current HEAD commit (attached or detached).
                self.current_head_commit_id(&conn, session_id)?
                    .ok_or_else(|| {
                        SessionError::Conflict(
                            "cannot create branch: no commits on current HEAD".to_string(),
                        )
                    })?
            }
        };

        // Insert or update the branch ref.
        conn.execute(
            "INSERT INTO graph_refs (session_id, name, kind, commit_id, updated_at)
             VALUES (?1, ?2, 'branch', ?3, ?4)
             ON CONFLICT(session_id, kind, name) DO UPDATE SET commit_id=?3, updated_at=?4",
            params![session_id, name, commit_id, now],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        // If this branch is the currently checked-out branch, also sync session head_commit_id.
        // On detached HEAD (head_ref IS NULL) or a non-current branch, this is a no-op.
        conn.execute(
            "UPDATE graph_sessions SET head_commit_id=?1, updated_at=?2 WHERE session_id=?3 AND head_ref=?4",
            params![commit_id, now, session_id, name],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        Ok(GraphRef {
            session_id: session_id.to_string(),
            name: name.to_string(),
            kind: GraphRefKind::Branch,
            commit_id: Some(commit_id),
            updated_at: now,
        })
    }

    pub(super) fn diff_impl(&self, session_id: &str, from: &str, to: &str) -> Result<GraphDiff> {
        let conn = self.conn()?;

        // Resolve both targets to commit IDs.
        let from_id = self.resolve_target_to_commit_id(&conn, session_id, from)?;
        let to_id = self.resolve_target_to_commit_id(&conn, session_id, to)?;

        // Build ancestor set for `from` (including from itself).
        let from_ancestors = self.collect_ancestors(&conn, session_id, &from_id)?;
        let to_ancestors = self.collect_ancestors(&conn, session_id, &to_id)?;

        // Find common ancestor: walk from `from` towards root, first commit
        // that appears in to_ancestors.
        let common_ancestor_id = {
            let mut current = Some(from_id.clone());
            let mut found = None;
            while let Some(cid) = current {
                if to_ancestors.contains(&cid) {
                    found = Some(cid);
                    break;
                }
                // Get parent of current.
                current = self
                    .get_parent_ids(&conn, session_id, &cid)?
                    .into_iter()
                    .next();
            }
            found
        };

        // Commits only in `from`: from_ancestors excluding to_ancestors,
        // but only those reachable from `from` up to the common ancestor.
        let only_in_from = self.collect_unique_entries(
            &conn,
            session_id,
            &from_id,
            common_ancestor_id.as_deref(),
            &to_ancestors,
        )?;

        let only_in_to = self.collect_unique_entries(
            &conn,
            session_id,
            &to_id,
            common_ancestor_id.as_deref(),
            &from_ancestors,
        )?;

        Ok(GraphDiff {
            from_commit_id: from_id,
            to_commit_id: to_id,
            common_ancestor_id,
            only_in_from,
            only_in_to,
        })
    }

    pub(super) fn reset_impl(&self, session_id: &str, target: &str) -> Result<ResetResult> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        // Get current HEAD info.
        let (head_ref, old_commit_id): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT head_ref, head_commit_id FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))?;

        let branch_name = match head_ref {
            Some(name) => name,
            None => {
                return Err(SessionError::Conflict(
                    "cannot reset while HEAD is detached; checkout a branch first".to_string(),
                ));
            }
        };

        // Resolve target to a commit ID.
        let target_commit_id = self.resolve_target_to_commit_id(&conn, session_id, target)?;

        // Verify the target commit exists.
        let exists: bool =
            conn.query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                params![target_commit_id, session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })? > 0;
        if !exists {
            return Err(SessionError::MissingCommit(target_commit_id));
        }

        // Begin immediate transaction for atomicity.
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        // Update the branch ref.
        let ref_result = conn.execute(
            "UPDATE graph_refs SET commit_id=?1, updated_at=?2 WHERE session_id=?3 AND kind='branch' AND name=?4",
            params![target_commit_id, now, session_id, branch_name],
        );
        if let Err(source) = ref_result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(SessionError::Database {
                path: self.db_path.clone(),
                source,
            });
        }

        // Update session head_commit_id.
        let session_result = conn.execute(
            "UPDATE graph_sessions SET head_commit_id=?1, updated_at=?2 WHERE session_id=?3",
            params![target_commit_id, now, session_id],
        );
        if let Err(source) = session_result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(SessionError::Database {
                path: self.db_path.clone(),
                source,
            });
        }

        conn.execute_batch("COMMIT")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        Ok(ResetResult {
            session_id: session_id.to_string(),
            branch: branch_name,
            old_commit_id,
            new_commit_id: target_commit_id,
        })
    }
}
