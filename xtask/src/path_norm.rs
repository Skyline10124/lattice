// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::{Component, Path, PathBuf};

/// 依赖路径检查用的路径规整，等价于 Python 的 `os.path.normpath`，不解析符号链接。
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
