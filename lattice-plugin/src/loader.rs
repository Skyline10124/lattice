use std::path::{Path, PathBuf};

use crate::bundle::PluginBundle;
use crate::manifest::{load_manifest, ManifestError};

#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("duplicate plugin name '{name}' in {first} and {second}")]
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
}

/// Load all declarative plugins from a directory tree.
///
/// A plugin lives in any `plugin.toml` below `dir`. Non-plugin files are
/// ignored. Bundles are sorted by plugin name for deterministic replacement.
pub fn load_dir(dir: &Path) -> Result<Vec<PluginBundle>, LoaderError> {
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut manifests = Vec::new();
    collect_manifests(dir, &mut manifests)?;
    manifests.sort();

    let mut bundles: Vec<(PathBuf, PluginBundle)> = Vec::with_capacity(manifests.len());
    for manifest in manifests {
        bundles.push((manifest.clone(), load_manifest(&manifest)?));
    }
    bundles.sort_by(|a, b| {
        a.1.meta
            .name
            .cmp(&b.1.meta.name)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut previous: Option<(String, PathBuf)> = None;
    for (path, bundle) in &bundles {
        if let Some((previous_name, previous_path)) = previous.as_ref() {
            if previous_name == &bundle.meta.name {
                return Err(LoaderError::DuplicateName {
                    name: bundle.meta.name.clone(),
                    first: previous_path.clone(),
                    second: path.clone(),
                });
            }
        }
        previous = Some((bundle.meta.name.clone(), path.clone()));
    }

    Ok(bundles.into_iter().map(|(_, bundle)| bundle).collect())
}

fn collect_manifests(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), LoaderError> {
    for entry in std::fs::read_dir(dir).map_err(|source| LoaderError::Io {
        path: dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| LoaderError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| LoaderError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            collect_manifests(&path, out)?;
        } else if path.file_name().and_then(|n| n.to_str()) == Some("plugin.toml") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_dir_ignores_missing_directory() {
        let dir = std::env::temp_dir().join("lattice_missing_plugin_dir_for_test");
        let bundles = load_dir(&dir).unwrap();
        assert!(bundles.is_empty());
    }

    #[test]
    fn load_dir_rejects_duplicate_local_plugin_names() {
        let root = std::env::temp_dir().join(format!(
            "lattice_duplicate_plugin_dir_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        let manifest = r#"
[plugin]
name = "local:duplicate"

[output]
parse = "text"
"#;
        std::fs::write(root.join("one/plugin.toml"), manifest).unwrap();
        std::fs::write(root.join("two/plugin.toml"), manifest).unwrap();

        let result = load_dir(&root);

        assert!(matches!(
            result,
            Err(LoaderError::DuplicateName { name, .. }) if name == "local:duplicate"
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}
