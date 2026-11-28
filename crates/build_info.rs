// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 两个二进制共用的构建期版本推导；库 crate 不持有工作区构建身份。

use std::{fs, path::Path, process::Command};

#[cfg(not(test))]
pub fn emit() {
    // 构建身份要反映工作区的任意改动，使用不存在的路径让每次构建重新查询 Git。
    println!("cargo:rerun-if-changed=__lattice_always_rerun_for_git_version__");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let info = derive(&root);
    println!("cargo:rustc-env=LATTICE_VERSION={}", info.version);
    println!("cargo:rustc-env=LATTICE_BUILD_ID={}", info.build_id);
}

#[derive(Debug, PartialEq, Eq)]
struct BuildInfo {
    version: String,
    build_id: String,
}

fn derive(workspace_root: &Path) -> BuildInfo {
    query(workspace_root).unwrap_or_else(|| BuildInfo {
        version: "alpha-未知".into(),
        build_id: "未知".into(),
    })
}

fn query(workspace_root: &Path) -> Option<BuildInfo> {
    // 源码包可能位于另一个 Git 仓库内，必须先核验本工作区自己的 .git。
    let git_meta = fs::symlink_metadata(workspace_root.join(".git")).ok()?;
    if !git_meta.is_dir() && !git_meta.is_file() {
        return None;
    }
    let log = git_output(
        workspace_root,
        &[
            "-c",
            "log.showSignature=false",
            "log",
            "-1",
            "--no-color",
            "--date=format-local:%Y-%m-%d",
            "--format=%H%n%cd",
            "HEAD",
        ],
    )?;
    let log = std::str::from_utf8(&log).ok()?;
    let mut lines = log.lines();
    let commit = lines.next()?;
    let date = lines.next()?;
    if lines.next().is_some()
        || commit.len() != 40
        || !commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        || date.len() != 10
        || !date.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 4 | 7) {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return None;
    }

    let status = git_output(
        workspace_root,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?;
    let build_id = if status.is_empty() {
        commit[..12].to_owned()
    } else {
        format!("{}+dirty", &commit[..12])
    };
    Some(BuildInfo {
        version: format!("alpha-{date}"),
        build_id,
    })
}

fn git_output(workspace_root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("--no-optional-locks")
        .args(args)
        .env("TZ", "UTC0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").expect("测试需要 PATH"))
            .env("HOME", root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "版本测试")
            .env("GIT_AUTHOR_EMAIL", "version-test@example.invalid")
            .env("GIT_COMMITTER_NAME", "版本测试")
            .env("GIT_COMMITTER_EMAIL", "version-test@example.invalid")
            .env("GIT_AUTHOR_DATE", "2026-11-20T12:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2026-11-25T23:30:00-02:00")
            .output()
            .expect("启动测试 Git 失败");
        assert!(
            output.status.success(),
            "测试 Git 命令失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("测试 Git 输出应为 UTF-8")
            .trim()
            .into()
    }

    fn repository() -> (TempDir, String) {
        let root = tempfile::tempdir().expect("创建测试仓库失败");
        git(root.path(), &["init", "-q"]);
        fs::write(root.path().join("tracked"), "原始内容").unwrap();
        fs::write(root.path().join(".gitignore"), b"ignored\n").unwrap();
        git(root.path(), &["add", "."]);
        let tree = git(root.path(), &["write-tree"]);
        let commit = git(root.path(), &["commit-tree", &tree, "-m", "版本测试夹具"]);
        git(root.path(), &["update-ref", "HEAD", &commit]);
        (root, commit)
    }

    #[test]
    fn clean_identity_uses_utc_committer_date_and_twelve_hex_digits() {
        let (root, commit) = repository();
        let info = derive(root.path());
        assert_eq!(info.version, "alpha-2026-11-26");
        assert_eq!(info.build_id, &commit[..12]);
        assert!(
            info.build_id
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
    }

    #[test]
    fn dirty_identity_appends_dirty_suffix_for_tracked_or_untracked_changes() {
        let (root, _) = repository();
        let clean = derive(root.path());
        let dirty = BuildInfo {
            version: clean.version.clone(),
            build_id: format!("{}+dirty", clean.build_id),
        };

        fs::write(root.path().join("tracked"), "改动一").unwrap();
        assert_eq!(derive(root.path()), dirty);
        fs::write(root.path().join("tracked"), "原始内容").unwrap();
        assert_eq!(derive(root.path()), clean);

        fs::write(root.path().join("ignored"), "忽略内容").unwrap();
        assert_eq!(derive(root.path()), clean);

        fs::write(root.path().join("untracked"), "未跟踪内容").unwrap();
        assert_eq!(derive(root.path()), dirty);

        fs::remove_file(root.path().join("untracked")).unwrap();
        fs::remove_file(root.path().join("ignored")).unwrap();
        assert_eq!(derive(root.path()), clean);
    }

    #[test]
    fn missing_git_directory_does_not_use_an_outer_repository() {
        let plain = tempfile::tempdir().unwrap();
        let unknown = BuildInfo {
            version: "alpha-未知".into(),
            build_id: "未知".into(),
        };
        assert_eq!(derive(plain.path()), unknown);
        let (outer, _) = repository();
        let inner = outer.path().join("source-package");
        fs::create_dir(&inner).unwrap();
        assert_eq!(derive(&inner), unknown);
    }

    #[test]
    fn git_failure_falls_back_to_unknown_identity() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        assert_eq!(
            derive(root.path()),
            BuildInfo {
                version: "alpha-未知".into(),
                build_id: "未知".into()
            }
        );
    }
}
