use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use notify::{EventKind, Watcher as _};
use tracing::warn;

use crate::loader::load_dir;
use crate::registry::PluginRegistry;

/// Watches a plugin directory and atomically refreshes PluginRegistry.
///
/// Hot reload is run-level safe: already-cloned plugin bundles continue to run;
/// future registry lookups see the updated bundle set.
pub struct PluginWatcher {
    _handle: thread::JoinHandle<()>,
}

impl PluginWatcher {
    pub fn spawn(
        dir: PathBuf,
        registry: Arc<PluginRegistry>,
        _include_builtins: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, rx) = std::sync::mpsc::channel();

        let mut watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let is_relevant = event.paths.iter().any(|p| {
                        matches!(
                            p.extension().and_then(|ext| ext.to_str()),
                            Some("toml" | "md" | "json" | "txt")
                        )
                    });
                    if is_relevant {
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
            let _watcher = watcher;
            let mut local_names: std::collections::HashSet<String> =
                match load_registry_bundles(&dir, false) {
                    Ok(bundles) => bundles
                        .iter()
                        .map(|bundle| bundle.meta.name.clone())
                        .collect(),
                    Err(e) => {
                        warn!("Failed to read initial plugin directory state: {e}");
                        std::collections::HashSet::new()
                    }
                };
            for () in rx {
                match load_registry_bundles(&dir, false) {
                    Ok(bundles) => {
                        if _include_builtins {
                            let next_names: std::collections::HashSet<String> = bundles
                                .iter()
                                .map(|bundle| bundle.meta.name.clone())
                                .collect();
                            for removed in local_names.difference(&next_names) {
                                registry.remove(removed);
                            }
                            for bundle in bundles {
                                if let Err(e) = registry.replace(bundle) {
                                    warn!("Failed to hot-reload plugin registry: {e}");
                                }
                            }
                            local_names = next_names;
                        } else if let Err(e) = registry.replace_all(bundles) {
                            warn!("Failed to hot-reload plugin registry: {e}");
                        }
                    }
                    Err(e) => warn!("Failed to hot-reload plugin directory: {e}"),
                }
            }
        });

        Ok(Self { _handle: handle })
    }
}

pub fn load_registry_bundles(
    dir: &std::path::Path,
    _include_builtins: bool,
) -> Result<Vec<crate::bundle::PluginBundle>, crate::loader::LoaderError> {
    let mut bundles: Vec<crate::bundle::PluginBundle> = Vec::new();
    let local_bundles = load_dir(dir)?;
    for bundle in local_bundles {
        if let Some(index) = bundles.iter().position(|b| b.meta.name == bundle.meta.name) {
            bundles[index] = bundle;
        } else {
            bundles.push(bundle);
        }
    }
    Ok(bundles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_registry_bundles_handles_missing_directory() {
        let dir = std::env::temp_dir().join("lattice_missing_watch_dir_for_test");
        let bundles = load_registry_bundles(&dir, true).unwrap();
        assert!(bundles.is_empty());
    }

    #[test]
    fn local_manifest_loads_bundle_name_once() {
        let root = std::env::temp_dir().join(format!(
            "lattice_local_plugin_dir_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("override")).unwrap();
        std::fs::write(
            root.join("override/plugin.toml"),
            r#"
[plugin]
name = "LocalPlugin"
description = "local manifest"

[output]
parse = "text"
"#,
        )
        .unwrap();

        let bundles = load_registry_bundles(&root, true).unwrap();
        let matches: Vec<_> = bundles
            .iter()
            .filter(|bundle| bundle.meta.name == "LocalPlugin")
            .collect();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].meta.description, "local manifest");
        let _ = std::fs::remove_dir_all(&root);
    }
}
