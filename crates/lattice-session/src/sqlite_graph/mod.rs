//! SQLite graph store for sessions, commits, messages, and refs.
//!
//! Implements `SessionGraphStore` over SQLite with a single shared
//! connection, WAL journaling, and lazy schema initialization.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

use crate::{Result, SessionError};

mod commit;
mod helpers;
mod refs;
mod schema;
mod session;

#[cfg(test)]
mod tests;

pub(crate) const DEFAULT_BRANCH: &str = "main";

/// Maximum number of `?` bind parameters per single `IN (...)` clause.
/// SQLite's default compile-time limit is 999 (`SQLITE_MAX_VARIABLE_NUMBER`);
/// newer builds may raise it, but we cannot assume the deployment target does.
/// Queries with more IDs are split into chunks of this size and merged.
pub(super) const SQLITE_BIND_CHUNK_SIZE: usize = 900;

/// SQLite-backed session graph store.
///
/// Uses a single shared connection guarded by a Mutex. The connection is
/// opened lazily on first access (so `new` remains infallible) and runs
/// schema DDL plus PRAGMA journal_mode=WAL once on initialization.
pub struct SqliteSessionGraphStore {
    pub(crate) dir: PathBuf,
    pub(crate) db_path: PathBuf,
    pub(crate) conn: Mutex<Option<Connection>>,
}

/// RAII handle around the shared connection. Derefs to `Connection`, so
/// existing call sites can use `self.conn()?.execute(...)` unchanged.
pub(crate) struct ConnHandle<'a> {
    guard: MutexGuard<'a, Option<Connection>>,
}

impl<'a> std::ops::Deref for ConnHandle<'a> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.guard
            .as_ref()
            .expect("connection initialized by SqliteSessionGraphStore::conn()")
    }
}

impl SqliteSessionGraphStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let db_path = dir.join("session_v11.db");
        Self {
            dir,
            db_path,
            conn: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Acquire a locked handle to the shared connection, initializing on
    /// first call (creates the directory, opens the DB, runs schema DDL,
    /// enables WAL journaling).
    pub(crate) fn conn(&self) -> Result<ConnHandle<'_>> {
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            crate::create_dir_all(&self.dir)?;
            let conn =
                Connection::open(&self.db_path).map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            self.init_schema(&conn)?;
            *guard = Some(conn);
        }
        Ok(ConnHandle { guard })
    }
}
