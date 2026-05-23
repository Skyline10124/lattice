use std::path::{Path, PathBuf};

use super::DefaultToolExecutor;

impl DefaultToolExecutor {
    pub(super) fn resolve_user_path(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            Path::new(&self.project_root).join(path)
        }
    }

    pub(super) fn reject_parent_traversal(path: &str) -> Result<(), String> {
        for component in path.split('/') {
            if component == ".." {
                return Err(format!(
                    "path traversal denied: '{}' contains '..' component",
                    path
                ));
            }
        }
        for component in path.split('\\') {
            if component == ".." {
                return Err(format!(
                    "path traversal denied: '{}' contains '..' component",
                    path
                ));
            }
        }
        Ok(())
    }

    pub(super) fn ensure_under_project_root(&self, canonical: &Path) -> Result<(), String> {
        let root = Path::new(&self.project_root);
        if canonical == root {
            return Ok(());
        }
        if !canonical.starts_with(root) {
            return Err(format!(
                "path '{}' is outside the project root '{}'",
                canonical.display(),
                self.project_root
            ));
        }
        Ok(())
    }
}
