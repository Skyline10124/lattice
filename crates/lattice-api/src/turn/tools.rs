// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 协调层的最小工具执行接缝与默认空实现。
//!
//! 协调层只在轮次开始前按会话与工作区打开一次 [`TurnTools`] 冻结工具目录，
//! 并在单个调用槽位持久提交为 `Started` 之后交接执行，不感知底层运行时或策略细节。

use std::sync::{Arc, atomic::AtomicBool};

use lattice_conversation::{SessionId, ToolError};
use lattice_session::store::StartedInvocation;
use tokio::time::Instant;

use crate::LocalWorkspaceRoot;

/// 单个工具在轮次冻结目录与模型请求中的公开定义。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub tool_identity: String,
}

/// 单次已开始调用的执行结果。
///
/// [`ToolOutcome::Deadline`] 表示本次执行因轮次 `deadline` 中止，协调层不提交终态槽位，
/// 直接关闭当前 Step，使该槽位保持持久 `Started` 并按 `Indeterminate(NoResult)` 读出。
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOutcome {
    Succeeded(serde_json::Value),
    Failed(ToolError),
    Cancelled,
    Deadline,
    Indeterminate,
}

/// 按轮次打开工具目录与执行句柄的可注入接口。
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn open_turn(
        &self,
        session_id: SessionId,
        workspace: &LocalWorkspaceRoot,
    ) -> crate::session::Result<Box<dyn TurnTools>>;
}

/// 绑定到单个轮次的冻结工具集与调用分发入口。
#[async_trait::async_trait]
pub trait TurnTools: Send + Sync {
    fn specs(&self) -> &[ToolSpec];

    async fn execute(
        &self,
        started: StartedInvocation,
        name: &str,
        arguments: serde_json::Value,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
    ) -> ToolOutcome;
}

/// 不暴露任何工具的生产默认执行器。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTools;

#[async_trait::async_trait]
impl ToolExecutor for NoTools {
    async fn open_turn(
        &self,
        _session_id: SessionId,
        _workspace: &LocalWorkspaceRoot,
    ) -> crate::session::Result<Box<dyn TurnTools>> {
        Ok(Box::new(Self))
    }
}

#[async_trait::async_trait]
impl TurnTools for NoTools {
    fn specs(&self) -> &[ToolSpec] {
        &[]
    }

    async fn execute(
        &self,
        _started: StartedInvocation,
        _name: &str,
        _arguments: serde_json::Value,
        _cancel: Arc<AtomicBool>,
        _deadline: Instant,
    ) -> ToolOutcome {
        // 空目录在注册阶段即按 InvalidBatch 拒绝任何工具调用；此处仅作防御性兜底。
        ToolOutcome::Indeterminate
    }
}

pub(crate) fn default_tool_executor() -> Arc<dyn ToolExecutor> {
    #[cfg(feature = "test-fake-tools")]
    if let Ok(raw) = std::env::var(fake::TEST_TOOLS_ENV) {
        return Arc::new(fake::FakeToolExecutor::from_env_value(&raw));
    }
    Arc::new(NoTools)
}

#[cfg(feature = "test-fake-tools")]
pub mod fake {
    //! 仅在显式开启 `test-fake-tools` 时编译的确定性假工具执行器。
    //!
    //! 进程启动环境设置 `LATTICE_TEST_TOOLS`（逗号分隔的工具名列表）时启用；
    //! 未设置该环境变量时仍注入 [`super::NoTools`]。单个调用的行为由 JSON 参数中的
    //! `"action"` 与可选标记路径字段控制：
    //! - 省略 `"action"` 或 `"action": "read"`：返回 `"value"`（默认固定只读对象）；
    //! - `"action": "append_marker"`：向 `"path"` 指定的绝对路径追加 `"line"`（默认 `<name>:<invocation_id>\n`）；
    //! - `"action": "block"`：阻塞等待 `cancel` 或 `deadline`，分别返回 `Cancelled` 或 `Deadline`；
    //! - `"action": "oversize"`：返回超过 64 KiB 上限的字符串载荷（`"bytes"` 默认 `MAX_TOOL_CONTENT_BYTES + 1`）；
    //! - `"action": "fail"`：返回 `Failed(ToolError { code, message })`；
    //! - `"action": "indeterminate"`：返回 `Indeterminate`。
    //!
    //! 任意 `"action"` 均可附带 `"ready_path"`（进入 `execute` 时写入就绪文件）或
    //! `"marker_path"`（进入 `execute` 时追加一行执行记录），用于无 `sleep` 同步与重放核验。

    use std::{
        io::Write,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use lattice_conversation::{SessionId, ToolError};
    use lattice_session::store::{MAX_TOOL_CONTENT_BYTES, StartedInvocation};
    use tokio::time::Instant;

    use super::{ToolExecutor, ToolOutcome, ToolSpec, TurnTools};
    use crate::LocalWorkspaceRoot;

    pub const TEST_TOOLS_ENV: &str = "LATTICE_TEST_TOOLS";

    const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(5);

    #[derive(Clone, Debug)]
    pub struct FakeToolExecutor {
        specs: Vec<ToolSpec>,
    }

    impl FakeToolExecutor {
        pub fn new<I, S>(names: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: AsRef<str>,
        {
            let specs = names
                .into_iter()
                .map(|raw| {
                    let name = raw.as_ref().to_owned();
                    ToolSpec {
                        description: format!("测试假工具 {name}"),
                        input_schema: serde_json::json!({
                            "type": "object",
                            "additionalProperties": true,
                        }),
                        tool_identity: format!("fake:{name}@1"),
                        name,
                    }
                })
                .collect();
            Self { specs }
        }

        pub fn from_env_value(raw: &str) -> Self {
            Self::new(
                raw.split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty()),
            )
        }
    }

    #[async_trait::async_trait]
    impl ToolExecutor for FakeToolExecutor {
        async fn open_turn(
            &self,
            _session_id: SessionId,
            _workspace: &LocalWorkspaceRoot,
        ) -> crate::session::Result<Box<dyn TurnTools>> {
            Ok(Box::new(FakeTurnTools {
                specs: self.specs.clone(),
            }))
        }
    }

    struct FakeTurnTools {
        specs: Vec<ToolSpec>,
    }

    #[async_trait::async_trait]
    impl TurnTools for FakeTurnTools {
        fn specs(&self) -> &[ToolSpec] {
            &self.specs
        }

        async fn execute(
            &self,
            started: StartedInvocation,
            name: &str,
            arguments: serde_json::Value,
            cancel: Arc<AtomicBool>,
            deadline: Instant,
        ) -> ToolOutcome {
            let invocation_id = started.binding().invocation_id;
            let default_line = format!("{name}:{invocation_id}\n");

            if let Some(ready_value) = arguments.get("ready_path") {
                let Some(ready_path) = ready_value.as_str() else {
                    return invalid_arg("ready_path 必须是绝对路径字符串");
                };
                if let Err(outcome) =
                    write_marker(PathBuf::from(ready_path), default_line.clone(), false).await
                {
                    return outcome;
                }
            }

            if let Some(marker_value) = arguments.get("marker_path") {
                let Some(marker_path) = marker_value.as_str() else {
                    return invalid_arg("marker_path 必须是绝对路径字符串");
                };
                let line = arguments
                    .get("marker_line")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| default_line.clone());
                if let Err(outcome) = write_marker(PathBuf::from(marker_path), line, true).await {
                    return outcome;
                }
            }

            match arguments
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("read")
            {
                "read" => {
                    ToolOutcome::Succeeded(arguments.get("value").cloned().unwrap_or_else(|| {
                        serde_json::json!({
                            "tool": name,
                            "content": "fake-readonly-content",
                        })
                    }))
                }
                "append_marker" => {
                    let Some(path) = arguments.get("path").and_then(serde_json::Value::as_str)
                    else {
                        return invalid_arg("append_marker 需要提供绝对路径字符串 `path`");
                    };
                    let line = arguments
                        .get("line")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                        .unwrap_or(default_line);
                    match write_marker(PathBuf::from(path), line, true).await {
                        Ok(()) => ToolOutcome::Succeeded(serde_json::json!({ "appended": true })),
                        Err(outcome) => outcome,
                    }
                }
                "block" => loop {
                    if cancel.load(Ordering::Acquire) {
                        return ToolOutcome::Cancelled;
                    }
                    if Instant::now() >= deadline {
                        return ToolOutcome::Deadline;
                    }
                    tokio::select! {
                        biased;
                        _ = tokio::time::sleep_until(deadline) => {
                            return if cancel.load(Ordering::Acquire) {
                                ToolOutcome::Cancelled
                            } else {
                                ToolOutcome::Deadline
                            };
                        }
                        _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {}
                    }
                },
                "oversize" => {
                    let bytes = arguments
                        .get("bytes")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(MAX_TOOL_CONTENT_BYTES + 1);
                    ToolOutcome::Succeeded(serde_json::json!({
                        "payload": "x".repeat(bytes),
                    }))
                }
                "fail" => {
                    let code = arguments
                        .get("code")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("fake_error")
                        .to_owned();
                    let message = arguments
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("fake tool failed")
                        .to_owned();
                    ToolOutcome::Failed(ToolError { code, message })
                }
                "indeterminate" => ToolOutcome::Indeterminate,
                other => invalid_arg(&format!("未知假工具动作 `{other}`")),
            }
        }
    }

    fn invalid_arg(message: &str) -> ToolOutcome {
        ToolOutcome::Failed(ToolError {
            code: "invalid_fake_arguments".into(),
            message: message.to_owned(),
        })
    }

    async fn write_marker(path: PathBuf, content: String, append: bool) -> Result<(), ToolOutcome> {
        if !Path::new(&path).is_absolute() {
            return Err(invalid_arg("标记文件路径必须是绝对路径"));
        }
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    ToolOutcome::Failed(ToolError {
                        code: "marker_io_error".into(),
                        message: format!("创建假工具标记目录失败：{error}"),
                    })
                })?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.create(true).write(true);
            if append {
                options.append(true);
            } else {
                options.truncate(true);
            }
            let mut file = options.open(&path).map_err(|error| {
                ToolOutcome::Failed(ToolError {
                    code: "marker_io_error".into(),
                    message: format!("打开假工具标记文件失败：{error}"),
                })
            })?;
            file.write_all(content.as_bytes()).map_err(|error| {
                ToolOutcome::Failed(ToolError {
                    code: "marker_io_error".into(),
                    message: format!("写入假工具标记文件失败：{error}"),
                })
            })?;
            file.sync_all().map_err(|error| {
                ToolOutcome::Failed(ToolError {
                    code: "marker_io_error".into(),
                    message: format!("同步假工具标记文件失败：{error}"),
                })
            })
        })
        .await
        .map_err(|_| ToolOutcome::Indeterminate)?
    }
}
