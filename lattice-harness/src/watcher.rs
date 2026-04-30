use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use std::thread;

use notify::{EventKind, Watcher as _};
use tracing::warn;

use crate::registry::AgentRegistry;

// ---------------------------------------------------------------------------
// Watcher — notify-based hot reload for AgentRegistry
// ---------------------------------------------------------------------------

/// Spawns a background thread that watches the agent directory for changes
/// and atomically reloads the registry when an `agent.toml` file is added,
/// modified, or removed.
pub struct Watcher {
    _handle: thread::JoinHandle<()>,
}

impl Watcher {
    /// Start watching `dir`.  Any `agent.toml` change triggers a full reload
    /// of the registry at `target`.
    pub fn spawn(
        dir: PathBuf,
        target: Arc<RwLock<AgentRegistry>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, rx) = std::sync::mpsc::channel();

        let mut watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    // Only react to agent.toml file changes
                    let is_agent_toml = event
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(|n| n == "agent.toml").unwrap_or(false));
                    if is_agent_toml {
                        match event.kind {
                            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                                let _ = tx.send(());
                            }
                            _ => {}
                        }
                    }
                }
            })?;

        watcher.watch(&dir, notify::RecursiveMode::Recursive)?;

        let handle = thread::spawn(move || {
            // Keep `watcher` alive in this thread
            let _w = watcher;
            for () in rx {
                match AgentRegistry::load_dir(&dir) {
                    Ok(updated) => {
                        if let Ok(mut registry) = target.write() {
                            *registry = updated;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to hot-reload agent registry: {e}");
                    }
                }
            }
        });

        Ok(Self { _handle: handle })
    }
}

/// Load a registry from `~/.lattice/agents/` (or override via `LATTICE_AGENTS_DIR`).
pub fn default_agents_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LATTICE_AGENTS_DIR") {
        Path::new(&dir).to_path_buf()
    } else {
        dirs_override()
    }
}

fn dirs_override() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        Path::new(&home).join(".lattice").join("agents")
    } else {
        PathBuf::from(".lattice/agents")
    }
}
