// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 守护进程内置 IM 组件宿主（`0700` 目录准备、`inotify` 监视恢复与只读挂载构造）。

use std::{
    ffi::OsStr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use lattice_im::{ImBuildOutcome, prepare_im_gateway};
use lattice_server::{
    ComponentBuildOutcome, ComponentRegistration, ComponentRemountHandle, PreparedComponent,
    Service,
};
use lattice_store::{PrivateDirectory, PrivateFileError, StateLayout};

use crate::im_watch::ImDirectoryWatcher;

const CONFIG_ROOT_NAME: &str = ".lattice";
const IM_SUBDIR_NAME: &str = "im";

/// 守护进程侧的 `"im"` 组件宿主。
#[derive(Clone)]
pub(crate) struct ImHost {
    layout: StateLayout,
    home_dir: Option<PathBuf>,
    watcher: Arc<ImDirectoryWatcher>,
}

impl ImHost {
    pub(crate) fn new(layout: StateLayout, home_dir: Option<PathBuf>) -> Self {
        Self {
            layout,
            home_dir,
            watcher: ImDirectoryWatcher::new(),
        }
    }

    /// 构造传给 `lattice_server::serve` 的 `"im"` 组件注册项。
    pub(crate) fn registration(&self) -> ComponentRegistration {
        let prepare_host = self.clone();
        let build_host = self.clone();
        ComponentRegistration::new("im", move |service| build_host.build(service))
            .with_prepare(move |handle| prepare_host.prepare(handle))
    }

    fn resolve_config_dir(&self) -> Result<PathBuf, String> {
        let Some(home) = &self.home_dir else {
            return Err("后台环境未设置 HOME，无法定位 IM 配置目录".into());
        };
        if home.as_os_str().is_empty() {
            return Err("后台环境未设置 HOME，无法定位 IM 配置目录".into());
        }
        if !home.is_absolute() {
            return Err("后台环境 HOME 不是绝对路径，无法定位 IM 配置目录".into());
        }
        Ok(home.join(CONFIG_ROOT_NAME))
    }

    fn prepare(&self, remount_handle: &ComponentRemountHandle) -> Result<(), String> {
        let config_dir = match self.resolve_config_dir() {
            Ok(dir) => dir,
            Err(reason) => {
                self.watcher.clear();
                return Err(reason);
            }
        };

        let root_dir = match PrivateDirectory::open_or_create(&config_dir) {
            Ok(dir) => dir,
            Err(err) => {
                self.watcher.clear();
                return Err(map_prepare_private_error(PrepareRole::ConfigDir, err));
            }
        };

        let existed_before = match root_dir.child(OsStr::new(IM_SUBDIR_NAME), false) {
            Ok(existing) => existing.is_some(),
            Err(err) => {
                self.watcher.clear();
                return Err(map_prepare_private_error(PrepareRole::ImDir, err));
            }
        };

        if let Err(err) = root_dir.child(OsStr::new(IM_SUBDIR_NAME), true) {
            self.watcher.clear();
            return Err(map_prepare_private_error(PrepareRole::ImDir, err));
        }

        let im_dir = config_dir.join(IM_SUBDIR_NAME);
        let identity = match inspect_private_im_directory(&im_dir) {
            Ok(id) => id,
            Err(reason) => {
                self.watcher.clear();
                return Err(reason);
            }
        };

        if let Err(reason) =
            self.watcher
                .ensure_watching(&im_dir, identity, existed_before, remount_handle)
        {
            self.watcher.clear();
            return Err(reason);
        }

        Ok(())
    }

    fn build(&self, service: Service) -> ComponentBuildOutcome {
        let config_dir = match self.resolve_config_dir() {
            Ok(dir) => dir,
            Err(reason) => return ComponentBuildOutcome::InvalidConfiguration { reason },
        };

        match prepare_im_gateway(&config_dir, self.layout.clone(), service) {
            ImBuildOutcome::Prepared(gateway) => {
                ComponentBuildOutcome::Component(PreparedComponent::new(move |stop| async move {
                    gateway.run(stop).await.map_err(|err| err.to_string())
                }))
            }
            ImBuildOutcome::Disabled => ComponentBuildOutcome::Disabled,
            ImBuildOutcome::Unavailable { reason } => ComponentBuildOutcome::Unavailable { reason },
            ImBuildOutcome::InvalidConfiguration { reason } => {
                ComponentBuildOutcome::InvalidConfiguration { reason }
            }
        }
    }

    #[cfg(test)]
    fn active_watch_generation(&self) -> Option<u64> {
        self.watcher.active_generation()
    }
}

#[derive(Clone, Copy)]
enum PrepareRole {
    ConfigDir,
    ImDir,
}

fn inspect_private_im_directory(im_dir: &Path) -> Result<(u64, u64), String> {
    let meta = std::fs::symlink_metadata(im_dir)
        .map_err(|err| format!("im/config.toml：读取 im 配置目录元数据失败：{err}"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("im/config.toml：im 目录必须是真实目录且不得为符号链接".into());
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err("im/config.toml：配置文件及所属目录必须归当前用户所有".into());
    }
    if meta.mode() & 0o077 != 0 {
        return Err(
            "im/config.toml：im 目录权限过宽（不得向组或其他用户开放权限，须为 0700）".into(),
        );
    }
    Ok((meta.dev(), meta.ino()))
}

fn map_prepare_private_error(role: PrepareRole, error: PrivateFileError) -> String {
    match error {
        PrivateFileError::InvalidName => "im/config.toml：配置目录包含非法路径分量".into(),
        PrivateFileError::WrongOwner => {
            "im/config.toml：配置文件及所属目录必须归当前用户所有".into()
        }
        PrivateFileError::NotADirectory | PrivateFileError::DirectorySymlink => match role {
            PrepareRole::ConfigDir => {
                "im/config.toml：配置目录必须是真实目录且不得为符号链接".into()
            }
            PrepareRole::ImDir => "im/config.toml：im 目录必须是真实目录且不得为符号链接".into(),
        },
        PrivateFileError::BroadDirectoryPermissions => match role {
            PrepareRole::ConfigDir => {
                "im/config.toml：配置目录权限过宽（不得向组或其他用户开放权限，须为 0700）".into()
            }
            PrepareRole::ImDir => {
                "im/config.toml：im 目录权限过宽（不得向组或其他用户开放权限，须为 0700）".into()
            }
        },
        PrivateFileError::DirectoryDisappeared => "im/config.toml：配置目录在准备期间消失".into(),
        other => format!("im/config.toml：准备私有配置目录失败（{other}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{self, OpenOptions},
        io::Write as _,
        os::unix::fs::PermissionsExt,
        time::Duration,
    };

    use lattice_ipc::{
        ComponentState, DaemonStatus, ImMountResult, Request, Response,
        ipc::{ResponseReader, write_request},
    };
    use lattice_server::HostLifecycle;
    use tokio::{net::UnixStream, sync::mpsc};

    struct TestLifecycle;

    impl HostLifecycle for TestLifecycle {
        fn on_ready(&self) -> lattice_ipc::Result<()> {
            Ok(())
        }

        fn on_stopping(&self) {}
    }

    async fn ipc_call(layout: &StateLayout, request: &Request) -> lattice_ipc::Result<Response> {
        let sock_path = layout.lattice_root().join("local/api.sock");
        let mut stream = UnixStream::connect(&sock_path).await?;
        write_request(&mut stream, request).await?;
        ResponseReader::default().read(&mut stream).await
    }

    async fn wait_daemon_ready(layout: &StateLayout) -> DaemonStatus {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match ipc_call(layout, &Request::Status).await {
                Ok(Response::Status(status)) => return status,
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok(_) => panic!("等待测试后台就绪收到非 Status 响应"),
                Err(err) => panic!("等待测试后台就绪失败：{err:?}"),
            }
        }
    }

    async fn query_status(layout: &StateLayout) -> DaemonStatus {
        match ipc_call(layout, &Request::Status)
            .await
            .expect("Status 请求应成功")
        {
            Response::Status(status) => status,
            _ => panic!("预期 Response::Status"),
        }
    }

    async fn call_im_reload(layout: &StateLayout) -> ImMountResult {
        match ipc_call(layout, &Request::ImReload {})
            .await
            .expect("ImReload 请求应成功")
        {
            Response::ImReload(res) => res,
            _ => panic!("预期 Response::ImReload"),
        }
    }

    fn valid_config_toml(workspace: &Path) -> String {
        format!(
            "[telegram]\nallowed_senders = [10001]\nworkspace = '{}'\nagent = 'assistant'\n",
            workspace.display()
        )
    }

    async fn wait_until_component_matches(
        layout: &StateLayout,
        predicate: impl Fn(&[lattice_ipc::ComponentStatus]) -> bool,
        desc: &str,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let status = query_status(layout).await;
            if predicate(&status.components) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "等待组件状态满足 `{desc}` 超时，当前状态：{:?}",
                status.components
            );
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    #[tokio::test]
    async fn startup_creates_0700_dirs_and_rejects_broad_permissions_without_chmod() {
        // 1. 缺失 `.lattice` 与 `im/` 时启动自动以 0700 新建，无 config.toml 时 components 为空且监视已建立。
        let temp1 = tempfile::Builder::new()
            .prefix("ltc-im-")
            .tempdir_in("/tmp")
            .unwrap();
        let home1 = temp1.path().join("home");
        fs::create_dir(&home1).unwrap();
        fs::set_permissions(&home1, fs::Permissions::from_mode(0o700)).unwrap();
        let state_root1 = temp1.path().join("state");
        fs::create_dir(&state_root1).unwrap();
        fs::set_permissions(&state_root1, fs::Permissions::from_mode(0o700)).unwrap();
        let layout1 = StateLayout::in_state_root(&state_root1);
        let host1 = ImHost::new(layout1.clone(), Some(home1.clone()));
        let (stop_tx1, stop_rx1) = mpsc::channel(4);
        let serve_layout1 = layout1.clone();
        let reg1 = host1.registration();
        let task1 = tokio::spawn(async move {
            lattice_server::serve(
                serve_layout1,
                Some(home1),
                Duration::from_secs(5),
                stop_rx1,
                TestLifecycle,
                vec![reg1],
            )
            .await
        });

        let _ = wait_daemon_ready(&layout1).await;
        let deadline1 = tokio::time::Instant::now() + Duration::from_secs(5);
        while host1.active_watch_generation() != Some(1) {
            assert!(
                tokio::time::Instant::now() < deadline1,
                "启动成功准备后应在期限内建立第 1 代 inotify 监视"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let initial_status = query_status(&layout1).await;
        assert!(
            initial_status.components.is_empty(),
            "默认无 config.toml 时 IM 未启用，components 应为空"
        );
        let dot_lattice1 = temp1.path().join("home/.lattice");
        let im_dir1 = dot_lattice1.join("im");
        assert_eq!(
            fs::symlink_metadata(&dot_lattice1).unwrap().mode() & 0o777,
            0o700,
            "新建 .lattice 权限必须为 0700"
        );
        assert_eq!(
            fs::symlink_metadata(&im_dir1).unwrap().mode() & 0o777,
            0o700,
            "新建 .lattice/im 权限必须为 0700"
        );
        assert_eq!(
            host1.active_watch_generation(),
            Some(1),
            "启动成功准备后应建立第 1 代 inotify 监视"
        );

        stop_tx1.send(()).await.unwrap();
        task1.await.unwrap().unwrap();

        // 2. 已有 `im/` 目录权限过宽（0750）时，绝不自动 chmod，IM 列为未挂载且不建立监视。
        let temp2 = tempfile::Builder::new()
            .prefix("ltc-im-")
            .tempdir_in("/tmp")
            .unwrap();
        let home2 = temp2.path().join("home");
        let dot_lattice2 = home2.join(".lattice");
        let im_dir2 = dot_lattice2.join("im");
        fs::create_dir_all(&im_dir2).unwrap();
        fs::set_permissions(&home2, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&dot_lattice2, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&im_dir2, fs::Permissions::from_mode(0o750)).unwrap();

        let state_root2 = temp2.path().join("state");
        fs::create_dir(&state_root2).unwrap();
        fs::set_permissions(&state_root2, fs::Permissions::from_mode(0o700)).unwrap();
        let layout2 = StateLayout::in_state_root(&state_root2);
        let host2 = ImHost::new(layout2.clone(), Some(home2.clone()));
        let (stop_tx2, stop_rx2) = mpsc::channel(4);
        let serve_layout2 = layout2.clone();
        let reg2 = host2.registration();
        let task2 = tokio::spawn(async move {
            lattice_server::serve(
                serve_layout2,
                Some(home2),
                Duration::from_secs(5),
                stop_rx2,
                TestLifecycle,
                vec![reg2],
            )
            .await
        });

        let _ = wait_daemon_ready(&layout2).await;
        wait_until_component_matches(
            &layout2,
            |components| components.len() == 1,
            "初始目录权限核验失败反映为未挂载",
        )
        .await;
        let bad_perm_status = query_status(&layout2).await;
        assert_eq!(bad_perm_status.components.len(), 1);
        assert_eq!(bad_perm_status.components[0].name, "im");
        match &bad_perm_status.components[0].state {
            ComponentState::Unmounted { reason } => {
                assert!(
                    reason.contains("权限过宽"),
                    "未挂载原因应指出目录权限过宽，实际：{reason}"
                );
            }
            other => panic!("权限过宽应列为 Unmounted，实际：{other:?}"),
        }
        assert_eq!(
            fs::symlink_metadata(&im_dir2).unwrap().mode() & 0o777,
            0o750,
            "后台绝不能自动修改已有目录的过宽权限"
        );
        assert_eq!(
            host2.active_watch_generation(),
            None,
            "目录核验失败时不得建立 inotify 监视"
        );

        stop_tx2.send(()).await.unwrap();
        task2.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn inotify_triggers_only_on_closed_or_moved_config_and_reload_recovers_deleted_im_dir() {
        let temp = tempfile::Builder::new()
            .prefix("ltc-im-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();

        let state_root = temp.path().join("state");
        fs::create_dir(&state_root).unwrap();
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700)).unwrap();
        let layout = StateLayout::in_state_root(&state_root);
        let host = ImHost::new(layout.clone(), Some(home.clone()));
        let (stop_tx, stop_rx) = mpsc::channel(4);
        let serve_layout = layout.clone();
        let serve_home = Some(home.clone());
        let reg = host.registration();
        let task = tokio::spawn(async move {
            lattice_server::serve(
                serve_layout,
                serve_home,
                Duration::from_secs(5),
                stop_rx,
                TestLifecycle,
                vec![reg],
            )
            .await
        });

        let _ = wait_daemon_ready(&layout).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while host.active_watch_generation() != Some(1) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "启动后应在期限内建立第 1 代 inotify 监视"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let initial = query_status(&layout).await;
        assert!(initial.components.is_empty());
        assert_eq!(host.active_watch_generation(), Some(1));

        let im_dir = home.join(".lattice/im");
        let config_path = im_dir.join("config.toml");

        // 1. 打开 `config.toml` 写入但尚未关闭文件描述符：不得触发重新挂载。
        let mut open_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&config_path)
            .unwrap();
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
        open_file
            .write_all(valid_config_toml(&workspace).as_bytes())
            .unwrap();
        open_file.flush().unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            query_status(&layout).await.components.is_empty(),
            "写入未关闭前不应触发 inotify 重新挂载"
        );

        // 2. 关闭写入句柄触发 `CLOSE_WRITE`：自动重新挂载为缺少 Telegram 凭据的 `Unmounted`。
        drop(open_file);
        wait_until_component_matches(
            &layout,
            |components| {
                matches!(
                    components,
                    [c] if c.name == "im"
                        && matches!(&c.state, ComponentState::Unmounted { reason } if reason.contains("缺少 Telegram 平台凭据"))
                )
            },
            "CLOSE_WRITE 触发缺凭据未挂载",
        )
        .await;

        // 3. 删除 `config.toml`（`DELETE`）：不触发重新挂载，组件保持刚才的 `Unmounted` 快照。
        fs::remove_file(&config_path).unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            query_status(&layout).await.components.len(),
            1,
            "删除 config.toml 不应自动触发重新挂载"
        );

        // 4. 改写 `im/auth.json` 或临时文件：不触发重新挂载（若触发会因无 config.toml 而变成 Disabled 空列表）。
        let temp_other = im_dir.join(".editor.swp");
        fs::write(&temp_other, b"swap").unwrap();
        let auth_file = im_dir.join("auth.json");
        fs::write(&auth_file, b"{}").unwrap();
        fs::set_permissions(&auth_file, fs::Permissions::from_mode(0o600)).unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            query_status(&layout).await.components.len(),
            1,
            "修改 auth.json 或其他临时文件不应触发 inotify 重新挂载"
        );
        fs::remove_file(&temp_other).unwrap();
        fs::remove_file(&auth_file).unwrap();

        // 5. 编辑器临时文件原子改名到 `config.toml`（`MOVED_TO`）：写入空配置使 IM 自动变为 `Disabled`（components 变空）。
        let atomic_tmp = im_dir.join(".config.toml.tmp");
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&atomic_tmp)
                .unwrap();
            fs::set_permissions(&atomic_tmp, fs::Permissions::from_mode(0o600)).unwrap();
            f.write_all(b"# disabled\n").unwrap();
        }
        fs::rename(&atomic_tmp, &config_path).unwrap();
        wait_until_component_matches(
            &layout,
            |components| components.is_empty(),
            "MOVED_TO 空配置使 components 变为空",
        )
        .await;

        // 6. `rm -rf ~/.lattice/im` 后监视按代次失效，且不自动重建 `im/` 目录。
        fs::remove_dir_all(&im_dir).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while host.active_watch_generation().is_some() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "删除 im/ 目录后 inotify 监视代次应自动失效"
            );
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        assert!(
            !im_dir.exists(),
            "监视失效后后台不得自动重建被删除的 im/ 目录"
        );

        // 7. 调用 `ImReload` 重建 0700 `im/` 并建立新代次监视，随后写入 `config.toml` 再次自动触发重新挂载。
        let reload_res = call_im_reload(&layout).await;
        assert_eq!(reload_res, ImMountResult::Disabled);
        assert_eq!(
            fs::symlink_metadata(&im_dir).unwrap().mode() & 0o777,
            0o700,
            "ImReload 重建的 im/ 目录权限必须为 0700"
        );
        assert_eq!(
            host.active_watch_generation(),
            Some(2),
            "ImReload 恢复监视后代次应递增为 2"
        );

        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&config_path)
                .unwrap();
            fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
            f.write_all(valid_config_toml(&workspace).as_bytes())
                .unwrap();
        }
        wait_until_component_matches(
            &layout,
            |components| {
                matches!(
                    components,
                    [c] if c.name == "im"
                        && matches!(&c.state, ComponentState::Unmounted { reason } if reason.contains("缺少 Telegram 平台凭据"))
                )
            },
            "恢复监视后保存 config.toml 再次自动触发重新挂载",
        )
        .await;

        stop_tx.send(()).await.unwrap();
        task.await.unwrap().unwrap();
    }
}
