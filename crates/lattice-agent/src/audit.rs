//! Audit log for security-relevant events.
//!
//! Append-only JSONL with rolling files. Non-blocking — I/O errors are logged
//! via `tracing::error!` but do not block tool execution (audit is best-effort).

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// AuditLog
// ---------------------------------------------------------------------------

/// Append-only JSONL audit log with rolling file support.
///
/// Writes one JSON object per line. When the current file exceeds `max_size`,
/// a new file is created and old files beyond `max_files` are deleted.
/// All writes are non-blocking and I/O errors are logged via `tracing::error!`
/// without blocking tool execution — audit logging is best-effort by design.
pub struct AuditLog {
    dir: PathBuf,
    current_path: PathBuf,
    max_size: u64,    // default 10 MB
    max_files: usize, // default 5
    writer: Mutex<Option<tokio::fs::File>>,
    pending_tasks: StdMutex<Vec<JoinHandle<()>>>,
}

impl AuditLog {
    /// Create a new audit log writing into `dir`. The directory is created
    /// if it does not exist. The active log file is `dir/audit.jsonl`.
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).ok();
        Self {
            current_path: dir.join("audit.jsonl"),
            dir,
            max_size: 10 * 1024 * 1024, // 10 MB
            max_files: 5,
            writer: Mutex::new(None),
            pending_tasks: StdMutex::new(Vec::new()),
        }
    }

    /// Write one event as a JSON line. Non-blocking — I/O errors are logged
    /// via `tracing::error!` and the event is dropped. If the writer is not
    /// yet open, it is lazily created.
    pub async fn log(&self, event: AuditEvent) {
        let line = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut writer_guard = self.writer.lock().await;

        // Lazy-open the file on first write.
        if writer_guard.is_none() {
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.current_path)
                .await
            {
                Ok(f) => {
                    *writer_guard = Some(f);
                }
                Err(_) => return,
            }
        }

        if let Some(ref mut f) = *writer_guard {
            if let Err(e) = f.write_all(line.as_bytes()).await {
                tracing::error!("audit log write failed: {e}");
                return;
            }
            if let Err(e) = f.write_all(b"\n").await {
                tracing::error!("audit log newline write failed: {e}");
                return;
            }
            if let Err(e) = f.flush().await {
                tracing::error!("audit log flush failed: {e}");
            }
        }
    }

    /// Fire-and-forget log from a synchronous context.
    ///
    /// Spawns a Tokio task that writes the event. If no Tokio runtime is
    /// available the event is silently dropped.
    pub fn log_sync(self: &Arc<Self>, event: AuditEvent) {
        let this = Arc::clone(self);
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime.spawn(async move {
                this.log(event).await;
            }),
            Err(_) => {
                tracing::warn!("audit log event dropped because no Tokio runtime is active");
                return;
            }
        };
        match self.pending_tasks.lock() {
            Ok(mut pending) => pending.push(handle),
            Err(_) => {
                handle.abort();
                tracing::warn!("audit log background task queue is poisoned");
            }
        }
    }

    /// Check completed background writes spawned by `log_sync()`.
    pub async fn reap_background_tasks(&self) {
        let finished = match self.pending_tasks.lock() {
            Ok(mut pending) => {
                let mut finished = Vec::new();
                let mut open = Vec::with_capacity(pending.len());
                for handle in pending.drain(..) {
                    if handle.is_finished() {
                        finished.push(handle);
                    } else {
                        open.push(handle);
                    }
                }
                *pending = open;
                finished
            }
            Err(_) => {
                tracing::warn!("audit log background task queue is poisoned");
                return;
            }
        };

        for handle in finished {
            if let Err(e) = handle.await {
                tracing::warn!("audit log background write failed: {e}");
            } else {
                tracing::trace!("audit log background write completed");
            }
        }
    }

    /// Rotate the current log file if it exists and exceeds `max_size`.
    /// Old files beyond `max_files` are deleted.
    ///
    /// Rotation strategy:
    /// - `audit.jsonl`        → `audit.1.jsonl`
    /// - `audit.1.jsonl`      → `audit.2.jsonl`
    /// - ...
    /// - `audit.{max_files}.jsonl` → deleted
    pub async fn rotate(&self) {
        // Check current file size (best-effort — metadata errors are ignored).
        if let Ok(meta) = tokio::fs::metadata(&self.current_path).await {
            if meta.len() < self.max_size {
                return;
            }
        } else {
            return; // file doesn't exist yet, nothing to rotate
        }

        // Shift existing rotated files.
        for i in (1..self.max_files).rev() {
            let old_path = if i == 1 {
                self.dir.join("audit.jsonl")
            } else {
                self.dir.join(format!("audit.{}.jsonl", i - 1))
            };
            let new_path = self.dir.join(format!("audit.{}.jsonl", i));
            if let Err(e) = tokio::fs::rename(&old_path, &new_path).await {
                tracing::error!(
                    "audit log rotation rename failed for {}: {e}",
                    old_path.display()
                );
            }
        }

        // Rename current → audit.1.jsonl
        let backup = self.dir.join("audit.1.jsonl");
        if let Err(e) = tokio::fs::rename(&self.current_path, &backup).await {
            tracing::error!("audit log backup rename failed: {e}");
        }

        // Delete excess files.
        let excess = self.dir.join(format!("audit.{}.jsonl", self.max_files));
        if let Err(e) = tokio::fs::remove_file(&excess).await {
            tracing::error!("audit log excess file removal failed: {e}");
        }

        // Reset writer — next write will create a fresh audit.jsonl.
        let mut writer_guard = self.writer.lock().await;
        *writer_guard = None;
    }
}

// ---------------------------------------------------------------------------
// AuditEvent
// ---------------------------------------------------------------------------

/// A security-relevant event recorded in the audit log.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "event")]
pub enum AuditEvent {
    /// A tool was executed (or blocked).
    #[serde(rename = "tool_exec")]
    ToolExec {
        ts: String,
        tool: String,
        command: String,
        /// "strict" | "permissive"
        sandbox: String,
        /// "ok" | "denied"
        result: String,
    },

    /// The agent's behavior mode was changed.
    #[serde(rename = "behavior_switch")]
    BehaviorSwitch {
        ts: String,
        from: String,
        to: String,
        /// "user_request" | "permissive_scope"
        trigger: String,
    },

    /// A HookChain hook denied a command.
    #[serde(rename = "hook_deny")]
    HookDeny {
        ts: String,
        hook: String,
        tool: String,
        command: String,
        reason: String,
        /// "Deny" | "Lockdown" | "Warning"
        severity: String,
    },
}

impl AuditEvent {
    /// Unix timestamp in `seconds.milliseconds` format.
    fn now_ts() -> String {
        let dur = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        format!("{}.{:03}", dur.as_secs(), dur.subsec_millis())
    }

    pub fn tool_exec(tool: &str, command: &str, sandbox: &str, result: &str) -> Self {
        AuditEvent::ToolExec {
            ts: Self::now_ts(),
            tool: tool.to_string(),
            command: command.to_string(),
            sandbox: sandbox.to_string(),
            result: result.to_string(),
        }
    }

    pub fn behavior_switch(from: &str, to: &str, trigger: &str) -> Self {
        AuditEvent::BehaviorSwitch {
            ts: Self::now_ts(),
            from: from.to_string(),
            to: to.to_string(),
            trigger: trigger.to_string(),
        }
    }

    pub fn hook_deny(hook: &str, tool: &str, command: &str, reason: &str, severity: &str) -> Self {
        AuditEvent::HookDeny {
            ts: Self::now_ts(),
            hook: hook.to_string(),
            tool: tool.to_string(),
            command: command.to_string(),
            reason: reason.to_string(),
            severity: severity.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_exec_serialization() {
        let event = AuditEvent::tool_exec("bash", "ls -la", "strict", "ok");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"tool_exec""#));
        assert!(json.contains(r#""tool":"bash""#));
        assert!(json.contains(r#""command":"ls -la""#));
        assert!(json.contains(r#""sandbox":"strict""#));
        assert!(json.contains(r#""result":"ok""#));
        assert!(json.contains(r#""ts":""#));
    }

    #[test]
    fn test_behavior_switch_serialization() {
        let event = AuditEvent::behavior_switch("Strict", "Yolo", "permissive_scope");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"behavior_switch""#));
        assert!(json.contains(r#""from":"Strict""#));
        assert!(json.contains(r#""to":"Yolo""#));
        assert!(json.contains(r#""trigger":"permissive_scope""#));
    }

    #[test]
    fn test_hook_deny_serialization() {
        let event =
            AuditEvent::hook_deny("tirith", "bash", "rm -rf /", "rm -rf / blocked", "Lockdown");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"hook_deny""#));
        assert!(json.contains(r#""hook":"tirith""#));
        assert!(json.contains(r#""command":"rm -rf /""#));
        assert!(json.contains(r#""severity":"Lockdown""#));
    }

    #[tokio::test]
    async fn test_audit_log_write_and_read() {
        let dir = std::env::temp_dir().join(format!("lattice-audit-test-{}", uuid::Uuid::new_v4()));
        let log = AuditLog::new(dir.clone());

        log.log(AuditEvent::tool_exec("bash", "ls", "strict", "ok"))
            .await;
        log.log(AuditEvent::hook_deny(
            "tirith", "bash", "rm -rf /", "blocked", "Lockdown",
        ))
        .await;

        // Read back from the file.
        let path = dir.join("audit.jsonl");
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2, "should have 2 lines, got: {:?}", lines);

        // Parse each line as JSON.
        for line in &lines {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("each line should be valid JSON");
            assert!(v.get("event").is_some(), "missing event tag");
            assert!(v.get("ts").is_some(), "missing ts field");
        }

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_audit_log_lazy_open() {
        let dir = std::env::temp_dir().join(format!("lattice-audit-lazy-{}", uuid::Uuid::new_v4()));
        let log = AuditLog::new(dir.clone());

        // Verify the file does not exist before any writes.
        let path = dir.join("audit.jsonl");
        assert!(!path.exists(), "file should not exist before first write");

        log.log(AuditEvent::tool_exec("bash", "ls", "strict", "ok"))
            .await;

        assert!(path.exists(), "file should exist after first write");

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
