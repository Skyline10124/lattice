// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! IM 网关只读挂载构造与 `im/config.toml` 解析校验。
//!
//! 构造阶段仅读取本地私有文件并校验路径与格式，不启动异步任务、不解析 DNS、不发起网络连接，也不读写运行态台账。

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use lattice_ipc::AgentName;
use lattice_server::{ImAuthStore, ImToken, Service, TELEGRAM_PLATFORM};
use lattice_store::{PrivateDirectory, PrivateFileError, StateLayout};

use crate::{PreparedImGateway, identity::SenderKey, telegram};

const IM_SUBDIR_NAME: &str = "im";
const CONFIG_FILE_NAME: &str = "config.toml";
const MAX_CONFIG_BYTES: usize = 1_048_576;

/// 只读构造阶段解析通过的 Telegram 网关静态配置。
#[derive(Clone)]
pub(crate) struct TelegramGatewayConfig {
    pub(crate) allowed_senders: BTreeSet<SenderKey>,
    pub(crate) workspace: PathBuf,
    pub(crate) agent: AgentName,
    pub(crate) token: ImToken,
}

impl std::fmt::Debug for TelegramGatewayConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramGatewayConfig")
            .field("allowed_senders_count", &self.allowed_senders.len())
            .field("workspace", &self.workspace)
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

/// IM 网关只读挂载构造结果。
#[derive(Debug)]
pub enum ImBuildOutcome {
    /// 配置与平台凭据均已就绪，可交由宿主启动运行。
    Prepared(PreparedImGateway),
    /// 配置目录、`im/config.toml` 或 `[telegram]` 表不存在，IM 处于关闭状态。
    Disabled,
    /// 已启用 `[telegram]` 表，但缺少对应的平台凭据。
    Unavailable {
        /// 脱敏后的中文不可用原因。
        reason: String,
    },
    /// 配置文件、凭据文件或其所属目录权限、类型、路径或字段校验失败。
    InvalidConfiguration {
        /// 脱敏后的中文配置无效原因。
        reason: String,
    },
}

/// 读取 `<config_dir>/im/config.toml` 与 `<config_dir>/im/auth.json`，执行只读挂载构造。
pub fn prepare_im_gateway(
    config_dir: &Path,
    layout: StateLayout,
    service: Service,
) -> ImBuildOutcome {
    let parsed_table = match read_config_bytes(config_dir) {
        Ok(None) => return ImBuildOutcome::Disabled,
        Ok(Some(bytes)) => match parse_config_toml(&bytes) {
            Ok(None) => return ImBuildOutcome::Disabled,
            Ok(Some(table)) => table,
            Err(reason) => return ImBuildOutcome::InvalidConfiguration { reason },
        },
        Err(reason) => return ImBuildOutcome::InvalidConfiguration { reason },
    };

    let auth_store = ImAuthStore::new(config_dir.to_path_buf());
    let token = match auth_store.get(TELEGRAM_PLATFORM) {
        Ok(Some(token)) => token,
        Ok(None) => {
            return ImBuildOutcome::Unavailable {
                reason: "缺少 Telegram 平台凭据（im/auth.json）".into(),
            };
        }
        Err(error) => {
            return ImBuildOutcome::InvalidConfiguration {
                reason: error.to_string(),
            };
        }
    };

    let config = TelegramGatewayConfig {
        allowed_senders: parsed_table.allowed_senders,
        workspace: parsed_table.workspace,
        agent: parsed_table.agent,
        token,
    };
    ImBuildOutcome::Prepared(PreparedImGateway::new(config, layout, service))
}

struct ParsedTelegramTable {
    allowed_senders: BTreeSet<SenderKey>,
    workspace: PathBuf,
    agent: AgentName,
}

#[derive(Clone, Copy)]
enum ConfigPathRole {
    ConfigDir,
    ImDir,
    ConfigFile,
}

fn read_config_bytes(config_dir: &Path) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, String> {
    let Some(root_dir) = PrivateDirectory::open(config_dir)
        .map_err(|err| map_private_error(ConfigPathRole::ConfigDir, err))?
    else {
        return Ok(None);
    };
    let Some(im_dir) = root_dir
        .child(OsStr::new(IM_SUBDIR_NAME), false)
        .map_err(|err| map_private_error(ConfigPathRole::ImDir, err))?
    else {
        return Ok(None);
    };
    im_dir
        .read(OsStr::new(CONFIG_FILE_NAME), Some(MAX_CONFIG_BYTES))
        .map_err(|err| map_private_error(ConfigPathRole::ConfigFile, err))
}

fn map_private_error(role: ConfigPathRole, error: PrivateFileError) -> String {
    match error {
        PrivateFileError::InvalidName => "im/config.toml：配置目录或文件名包含非法路径分量".into(),
        PrivateFileError::WrongOwner => {
            "im/config.toml：配置文件及所属目录必须归当前用户所有".into()
        }
        PrivateFileError::NotADirectory | PrivateFileError::DirectorySymlink => match role {
            ConfigPathRole::ConfigDir => {
                "im/config.toml：配置目录必须是真实目录且不得为符号链接".into()
            }
            ConfigPathRole::ImDir | ConfigPathRole::ConfigFile => {
                "im/config.toml：im 目录必须是真实目录且不得为符号链接".into()
            }
        },
        PrivateFileError::BroadDirectoryPermissions => match role {
            ConfigPathRole::ConfigDir => {
                "im/config.toml：配置目录权限过宽（不得向组或其他用户开放权限，须为 0700）".into()
            }
            ConfigPathRole::ImDir | ConfigPathRole::ConfigFile => {
                "im/config.toml：im 目录权限过宽（不得向组或其他用户开放权限，须为 0700）".into()
            }
        },
        PrivateFileError::DirectoryDisappeared => "im/config.toml：配置目录在读取期间消失".into(),
        PrivateFileError::FileSymlink | PrivateFileError::NotARegularFile => {
            "im/config.toml：配置文件必须是普通文件且不得为符号链接".into()
        }
        PrivateFileError::BroadFilePermissions => {
            "im/config.toml：配置文件权限过宽（不得向组或其他用户开放权限，须为 0600）".into()
        }
        PrivateFileError::FileTooLarge { .. } => {
            "im/config.toml：配置文件大小超过 1 MiB 上限".into()
        }
        PrivateFileError::DirectorySyncAfterReplace { source }
        | PrivateFileError::Io { source, .. } => {
            format!(
                "im/config.toml：读取失败（{}）",
                io_kind_label(source.kind())
            )
        }
    }
}

fn io_kind_label(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => "路径不存在",
        std::io::ErrorKind::PermissionDenied => "权限不足",
        std::io::ErrorKind::NotADirectory => "目标不是目录",
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => "输入无效",
        _ => "系统 I/O 错误",
    }
}

fn parse_config_toml(bytes: &[u8]) -> Result<Option<ParsedTelegramTable>, String> {
    let raw = std::str::from_utf8(bytes)
        .map_err(|_| "im/config.toml：文件不是有效的 UTF-8 文本".to_owned())?;

    let value: toml::Value = toml::from_str(raw).map_err(|error| {
        if let Some(span) = error.span() {
            let line = raw.get(..span.start).map_or(1, |prefix| {
                prefix.bytes().filter(|&b| b == b'\n').count() + 1
            });
            format!("im/config.toml：TOML 语法或重复键错误（第 {line} 行）")
        } else {
            "im/config.toml：TOML 语法或重复键错误".to_owned()
        }
    })?;

    let toml::Value::Table(mut root) = value else {
        return Err("im/config.toml：顶层结构必须是 TOML 表".into());
    };

    for key in root.keys() {
        if key != TELEGRAM_PLATFORM {
            return Err("im/config.toml：包含未知顶层配置项（仅支持 [telegram]）".into());
        }
    }

    let Some(tg_value) = root.remove(TELEGRAM_PLATFORM) else {
        return Ok(None);
    };
    let toml::Value::Table(mut tg) = tg_value else {
        return Err("im/config.toml：[telegram] 必须是表".into());
    };

    for key in tg.keys() {
        if !matches!(key.as_str(), "allowed_senders" | "workspace" | "agent") {
            return Err(
                "im/config.toml：[telegram] 包含未知配置项（仅支持 allowed_senders、workspace、agent）"
                    .into(),
            );
        }
    }

    let senders_val = tg
        .remove("allowed_senders")
        .ok_or_else(|| "im/config.toml：[telegram].allowed_senders 缺失".to_owned())?;
    let toml::Value::Array(raw_senders) = senders_val else {
        return Err("im/config.toml：[telegram].allowed_senders 必须是整数数组".into());
    };
    let mut allowed_senders = BTreeSet::new();
    for item in raw_senders {
        let toml::Value::Integer(sender_id) = item else {
            return Err(
                "im/config.toml：[telegram].allowed_senders 元素必须是 64 位有符号整数".into(),
            );
        };
        allowed_senders.insert(telegram::sender_key_from_id(sender_id));
    }

    let workspace_val = tg
        .remove("workspace")
        .ok_or_else(|| "im/config.toml：[telegram].workspace 缺失".to_owned())?;
    let toml::Value::String(workspace_str) = workspace_val else {
        return Err("im/config.toml：[telegram].workspace 必须是绝对路径字符串".into());
    };
    let raw_workspace = Path::new(&workspace_str);
    if !raw_workspace.is_absolute() {
        return Err("im/config.toml：[telegram].workspace 必须是绝对路径".into());
    }
    let canonical_workspace = std::fs::canonicalize(raw_workspace).map_err(|err| {
        format!(
            "im/config.toml：[telegram].workspace 无法规范化（{}）",
            io_kind_label(err.kind())
        )
    })?;
    let workspace_meta = std::fs::metadata(&canonical_workspace).map_err(|err| {
        format!(
            "im/config.toml：[telegram].workspace 无法读取目录元数据（{}）",
            io_kind_label(err.kind())
        )
    })?;
    if !workspace_meta.is_dir() {
        return Err("im/config.toml：[telegram].workspace 必须指向已存在的目录".into());
    }

    let agent_val = tg
        .remove("agent")
        .ok_or_else(|| "im/config.toml：[telegram].agent 缺失".to_owned())?;
    let toml::Value::String(agent_str) = agent_val else {
        return Err("im/config.toml：[telegram].agent 必须是字符串".into());
    };
    let agent = AgentName::parse(&agent_str).map_err(|_| {
        "im/config.toml：[telegram].agent 名称格式无效（须为非空 ASCII 字母、数字、连字符或下划线，且不超过 64 字节）"
            .to_owned()
    })?;

    Ok(Some(ParsedTelegramTable {
        allowed_senders,
        workspace: canonical_workspace,
        agent,
    }))
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    use lattice_server::{ImAuthStore, serve_for_test_with_service};
    use tokio::sync::oneshot;

    use super::*;

    async fn with_dummy_service<F, Fut>(f: F)
    where
        F: FnOnce(PathBuf, StateLayout, Service) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let root = tempfile::tempdir().expect("创建测试临时目录应成功");
        let layout = StateLayout::in_state_root(root.path());
        let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(2);
        let (svc_tx, svc_rx) = oneshot::channel();
        let serving_layout = layout.clone();
        let home = Some(root.path().to_path_buf());
        let handle = tokio::spawn(async move {
            serve_for_test_with_service(
                serving_layout,
                home,
                Duration::from_secs(10),
                stop_rx,
                svc_tx,
            )
            .await
        });
        let service = svc_rx.await.expect("应成功获取测试 Service");
        f(root.path().to_path_buf(), layout, service).await;
        let _ = stop_tx.send(()).await;
        let exit = handle
            .await
            .expect("后台任务不应崩溃")
            .expect("后台退出不应报错");
        drop(exit.instance);
    }

    fn write_private_config(config_dir: &Path, content: &str) {
        let root = PrivateDirectory::open_or_create(config_dir).expect("创建私有配置目录应成功");
        let im_dir = root
            .child(OsStr::new("im"), true)
            .expect("创建私有 im 子目录应成功")
            .expect("create=true 必返回目录");
        im_dir
            .replace(OsStr::new("config.toml"), content.as_bytes())
            .expect("写入私有 config.toml 应成功");
    }

    #[tokio::test]
    async fn prepare_covers_disabled_unavailable_invalid_and_canonical_prepared() {
        with_dummy_service(|base, layout, service| async move {
            let config_dir = base.join("cfg");
            let workspace_real = base.join("ws-real");
            std::fs::create_dir_all(&workspace_real).expect("创建工作区目录应成功");
            let workspace_link = base.join("ws-link");
            std::os::unix::fs::symlink(&workspace_real, &workspace_link)
                .expect("创建工作区符号链接应成功");

            // 1. 配置目录不存在 -> Disabled（且不创建目录）
            assert!(
                matches!(
                    prepare_im_gateway(&config_dir, layout.clone(), service.clone()),
                    ImBuildOutcome::Disabled
                ),
                "缺失配置目录时应返回 Disabled"
            );
            assert!(!config_dir.exists(), "只读构造不得创建缺失的配置目录");

            // 2. 空文件或无 [telegram] 表 -> Disabled
            write_private_config(&config_dir, "# 尚未启用 Telegram\n");
            assert!(
                matches!(
                    prepare_im_gateway(&config_dir, layout.clone(), service.clone()),
                    ImBuildOutcome::Disabled
                ),
                "缺少 [telegram] 表时应返回 Disabled"
            );

            // 3. 合法 [telegram]（含空 allowed_senders 与符号链接工作区），但缺少凭据 -> Unavailable
            let valid_toml = format!(
                "[telegram]\nallowed_senders = []\nworkspace = '{}'\nagent = 'nonexistent-profile-ok'\n",
                workspace_link.display()
            );
            write_private_config(&config_dir, &valid_toml);
            match prepare_im_gateway(&config_dir, layout.clone(), service.clone()) {
                ImBuildOutcome::Unavailable { reason } => {
                    assert!(reason.contains("缺少 Telegram 平台凭据"), "{reason}");
                }
                other => panic!("缺少凭据应返回 Unavailable，实际: {other:?}"),
            }

            // 4. 写入合法凭据 -> Prepared，工作区保存为规范化真实路径，不检查 agent profile 文件是否存在
            ImAuthStore::new(config_dir.clone())
                .set(TELEGRAM_PLATFORM, "123456:TEST_TOKEN")
                .expect("写入测试凭据应成功");

            match prepare_im_gateway(&config_dir, layout.clone(), service.clone()) {
                ImBuildOutcome::Prepared(prepared) => {
                    assert_eq!(
                        prepared.config.workspace,
                        std::fs::canonicalize(&workspace_real).unwrap(),
                        "构造阶段应保存规范化后的工作区路径"
                    );
                    assert!(prepared.config.allowed_senders.is_empty());
                    assert_eq!(prepared.config.agent.as_str(), "nonexistent-profile-ok");
                }
                other => panic!("配置与凭据齐全时应返回 Prepared，实际: {other:?}"),
            }

            // 5. 各类非法配置：未知字段、allowed_senders 类型错误、相对路径、不存在路径、文件而非目录、非法 agent
            let plain_file = base.join("plain.txt");
            std::fs::write(&plain_file, "file").unwrap();
            let bad_cases = [
                (
                    "未知顶层表",
                    format!(
                        "[other]\nx = 1\n[telegram]\nallowed_senders = [1]\nworkspace = '{}'\nagent = 'a'\n",
                        workspace_real.display()
                    ),
                ),
                (
                    "未知 telegram 键",
                    format!(
                        "[telegram]\nallowed_senders = [1]\nworkspace = '{}'\nagent = 'a'\nextra = true\n",
                        workspace_real.display()
                    ),
                ),
                (
                    "allowed_senders 含字符串",
                    format!(
                        "[telegram]\nallowed_senders = ['123']\nworkspace = '{}'\nagent = 'a'\n",
                        workspace_real.display()
                    ),
                ),
                (
                    "相对路径 workspace",
                    "[telegram]\nallowed_senders = [1]\nworkspace = 'relative/path'\nagent = 'a'\n"
                        .to_owned(),
                ),
                (
                    "不存在的 workspace",
                    format!(
                        "[telegram]\nallowed_senders = [1]\nworkspace = '{}'\nagent = 'a'\n",
                        base.join("missing-ws").display()
                    ),
                ),
                (
                    "指向普通文件的 workspace",
                    format!(
                        "[telegram]\nallowed_senders = [1]\nworkspace = '{}'\nagent = 'a'\n",
                        plain_file.display()
                    ),
                ),
                (
                    "非法 agent 名",
                    format!(
                        "[telegram]\nallowed_senders = [1]\nworkspace = '{}'\nagent = '../bad agent'\n",
                        workspace_real.display()
                    ),
                ),
                ("TOML 语法错误且含秘密字面量", "broken = 'super-secret-value".to_owned()),
            ];

            for (label, content) in bad_cases {
                write_private_config(&config_dir, &content);
                match prepare_im_gateway(&config_dir, layout.clone(), service.clone()) {
                    ImBuildOutcome::InvalidConfiguration { reason } => {
                        assert!(
                            !reason.contains("super-secret-value"),
                            "{label} 报错不得回显原始秘密字面量: {reason}"
                        );
                    }
                    other => panic!("{label} 应返回 InvalidConfiguration，实际: {other:?}"),
                }
            }

            // 6. 合法 config.toml 但 auth.json 损坏或权限过宽 -> InvalidConfiguration
            write_private_config(&config_dir, &valid_toml);
            let auth_path = config_dir.join("im/auth.json");
            std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                matches!(
                    prepare_im_gateway(&config_dir, layout, service),
                    ImBuildOutcome::InvalidConfiguration { .. }
                ),
                "过宽权限的 auth.json 必须返回 InvalidConfiguration 而非 Unavailable"
            );
        })
        .await;
    }
}
