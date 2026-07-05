// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::{Component, Path, PathBuf};

/// `os.path.normpath` equivalent for dependency path checks (no symlink resolution).
pub fn normpath(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn normpath_collapses_parent_dirs() {
        let p = Path::new("/repo/crates/harness/../../crates/foo");
        assert_eq!(normpath(p), PathBuf::from("/repo/crates/foo"));
    }
}
