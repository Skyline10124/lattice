// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Git 配置隔离在独立进程中验证，避免修改其他测试的环境。

use std::{fs, process::Command};

use lattice_store::{ExpectedMeta, META_REF, ProjectStoreKey, StoreRepo, StoreScope};

#[test]
fn create_and_open_ignore_user_and_global_git_config() {
    const FIXTURE: &str = "LATTICE_STORE_GIT_ENV_FIXTURE";
    if let Some(root) = std::env::var_os(FIXTURE) {
        let root = std::path::PathBuf::from(root);
        let expected = ExpectedMeta {
            store_kind: "workspace-history".into(),
            store_schema_version: 1,
            scope: StoreScope::Project {
                key: ProjectStoreKey::from_canonical_path_bytes(b"/workspace"),
            },
        };
        let path = root.join("history.git");
        let created = StoreRepo::create(&path, (&expected).into(), |_| Ok(())).unwrap();
        // 打开时才引入命名空间；若读取用户配置，原有身份引用会变得不可见。
        if let Some(config) = std::env::var_os("LATTICE_STORE_GIT_CONFIG") {
            fs::write(
                config,
                "[gitoxide \"core\"]\n refsNamespace = hostile-open\n",
            )
            .unwrap();
        }
        let reopened = StoreRepo::open(&path, &expected).unwrap();
        assert_eq!(created.meta(), reopened.meta());
        assert_eq!(
            created.ref_target(META_REF).unwrap(),
            reopened.ref_target(META_REF).unwrap()
        );
        let snapshot = (
            fs::read(path.join("HEAD")).unwrap(),
            fs::read(path.join("config")).unwrap(),
            reopened.ref_target(META_REF).unwrap().unwrap().to_string(),
        );
        fs::write(
            root.join("result.json"),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let mut baseline = None;
    for source in ["none", "home", "global"] {
        let root = temp.path().join(source);
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let global = root.join("global.gitconfig");
        fs::write(&global, "").unwrap();
        // 非法分支名会使非隔离的初始化（含位置探测）失败。
        let hostile = "[init]\n defaultBranch = hostile branch\n[core]\n logAllRefUpdates = true\n";
        if source == "home" {
            fs::write(home.join(".gitconfig"), hostile).unwrap();
        } else if source == "global" {
            fs::write(&global, hostile).unwrap();
        }
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "create_and_open_ignore_user_and_global_git_config",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(FIXTURE, &root);
        if source != "home" {
            command.env("GIT_CONFIG_GLOBAL", &global);
        }
        match source {
            "home" => {
                command.env("LATTICE_STORE_GIT_CONFIG", home.join(".gitconfig"));
            }
            "global" => {
                command.env("LATTICE_STORE_GIT_CONFIG", &global);
            }
            _ => {}
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{source} config: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let snapshot = fs::read(root.join("result.json")).unwrap();
        if let Some(baseline) = &baseline {
            assert_eq!(
                &snapshot, baseline,
                "{source} config changed repository creation"
            );
        } else {
            baseline = Some(snapshot);
        }
    }
}
