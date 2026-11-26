// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice daemon install`：写出 `lattice.service` 并通过 `systemctl --user` 启用与启动后台。

use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, ExitStatus},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use clap::{ArgAction, Args};
use lattice_local::{
    DrainTimeout, LocalClient, LocalError, LocalErrorKind, StateLayout, StatusProbe,
    parse_drain_timeout, xdg_state_root,
};

/// `systemctl` 调用后探测状态根后台的单次截止时间；失败时用于诊断冲突，成功时用于核对协议兼容性。
const STATUS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// `TimeoutStopSec` 在排空上限之外附加的尾段收尾与停写余量（秒）。
const STOP_TIMEOUT_MARGIN_SECS: u64 = 30;
/// 用户级 systemd 服务单元名称。
const SERVICE_UNIT_NAME: &str = "lattice.service";

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Args, Debug, Clone)]
pub(crate) struct InstallArgs {
    /// 停机时等待在途操作排空的上限（非负整数，单位支持 ms、s、m，无单位按秒计；默认 60s）。
    #[arg(
        long = "drain-timeout",
        value_name = "时长",
        default_value = "60s",
        hide_default_value = true,
        allow_hyphen_values = true,
        value_parser = parse_drain_timeout
    )]
    pub(crate) drain_timeout: DrainTimeout,

    /// 显示帮助。
    #[arg(
        short = 'h',
        long = "help",
        action = ArgAction::Help,
        display_order = 1000
    )]
    help: Option<bool>,
}

pub(super) async fn run(args: InstallArgs) -> ExitCode {
    match execute_install(args.drain_timeout.as_duration()).await {
        Ok(unit_path) => {
            println!("已写入 unit 文件：{}", unit_path.display());
            println!("已启用并启动服务 {SERVICE_UNIT_NAME}。");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("错误：{}", error.message);
            ExitCode::from(exit_code_for_kind(error.kind))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceAction {
    Start,
    Restart,
}

impl ServiceAction {
    fn systemctl_verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Restart => "restart",
        }
    }

    fn success_message(self) -> &'static str {
        match self {
            Self::Start => "已启动服务 lattice.service。",
            Self::Restart => "已重启服务 lattice.service。",
        }
    }
}

pub(super) async fn run_start() -> ExitCode {
    run_service_action(ServiceAction::Start).await
}

pub(super) async fn run_restart() -> ExitCode {
    run_service_action(ServiceAction::Restart).await
}

async fn run_service_action(action: ServiceAction) -> ExitCode {
    match execute_service_action(action).await {
        Ok(()) => {
            println!("{}", action.success_message());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("错误：{}", error.message);
            ExitCode::from(exit_code_for_kind(error.kind))
        }
    }
}

async fn execute_service_action(action: ServiceAction) -> Result<(), LocalError> {
    let verb = action.systemctl_verb();
    let Err(reason) =
        run_systemctl(vec!["--user".into(), verb.into(), SERVICE_UNIT_NAME.into()]).await
    else {
        return verify_service_action_backend(action).await;
    };

    if is_unit_not_found().await {
        return Err(LocalError::new(
            LocalErrorKind::Internal,
            "尚未安装用户服务 lattice.service；请先执行 `lattice daemon install`",
        ));
    }

    let probe = match StateLayout::resolve() {
        Ok(layout) => {
            let client = LocalClient::new(layout);
            probe_running_backend(&client, STATUS_PROBE_TIMEOUT).await
        }
        Err(_) => BackendProbe::Unreachable,
    };
    let message = format_service_action_failure_diagnostic(action, &reason, probe);
    Err(LocalError::new(LocalErrorKind::Internal, &message))
}

/// `systemctl` 已成功执行 `start` 或 `restart` 后核对状态根后台；状态根无法解析时视为探测不到。
async fn verify_service_action_backend(action: ServiceAction) -> Result<(), LocalError> {
    let Ok(layout) = StateLayout::resolve() else {
        return Ok(());
    };
    let client = LocalClient::new(layout);
    match action {
        ServiceAction::Start => {
            verify_backend_after_success(&client, CompletedAction::Start, STATUS_PROBE_TIMEOUT)
                .await
        }
        ServiceAction::Restart => {
            let sibling_daemon = sibling_daemon_diagnostic_path();
            verify_backend_after_success(
                &client,
                CompletedAction::Restart {
                    sibling_daemon: sibling_daemon.as_deref(),
                },
                STATUS_PROBE_TIMEOUT,
            )
            .await
        }
    }
}

fn exit_code_for_kind(kind: LocalErrorKind) -> u8 {
    match kind {
        LocalErrorKind::BadRequest | LocalErrorKind::NotFound | LocalErrorKind::Conflict => 2,
        _ => 1,
    }
}

async fn execute_install(drain_timeout: Duration) -> Result<PathBuf, LocalError> {
    let current_exe = std::env::current_exe().map_err(|error| {
        LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("无法定位当前 `lattice` 可执行文件路径：{error}"),
        )
    })?;
    let daemon_path = resolve_sibling_daemon(&current_exe)?;

    // 在触碰磁盘前先完成时长溢出检查、状态根解析、unit 路径解析与 `ExecStart` 转义校验。
    let state_root = xdg_state_root().map_err(|_| {
        LocalError::new(
            LocalErrorKind::BadRequest,
            "无法解析状态根目录：请设置绝对路径的 XDG_STATE_HOME 或 HOME",
        )
    })?;
    let unit_path = resolve_unit_path_from_env(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    let unit_contents = render_unit(&daemon_path, &state_root, drain_timeout)?;

    write_unit_atomically(&unit_path, &unit_contents)?;

    if let Err(reason) = run_systemctl(vec!["--user".into(), "daemon-reload".into()]).await {
        return Err(LocalError::new(
            LocalErrorKind::Internal,
            &format!(
                "已写入 unit 文件 {}，但执行 `systemctl --user daemon-reload` 失败（{reason}）",
                unit_path.display()
            ),
        ));
    }

    // 按 unit 文件的绝对路径启用：运行中的 user manager 按它自己的环境确定 unit 搜索路径，
    // 命令行的 `XDG_CONFIG_HOME` 与之不同时，写出的 unit 不在搜索路径里，按名称启用会报 unit 不存在。
    // 按路径启用时，systemd 先把文件链接进 manager 的配置目录再启用；文件已在搜索路径里时与按名称启用相同。
    let enable_args = vec![
        "--user".into(),
        "enable".into(),
        OsString::from(unit_path.as_os_str()),
    ];
    if let Err(reason) = run_systemctl(enable_args).await {
        return Err(LocalError::new(
            LocalErrorKind::Internal,
            &format!(
                "已写入 unit 文件 {}，但按该文件路径执行 `systemctl --user enable` 失败（{reason}）",
                unit_path.display()
            ),
        ));
    }

    // 用 restart 而不是 `enable --now`：服务尚未运行时等价于 start，服务已在运行旧版本或旧参数时会优雅重启到最新配置。
    let restart_args = vec!["--user".into(), "restart".into(), SERVICE_UNIT_NAME.into()];
    if let Err(reason) = run_systemctl(restart_args).await {
        let client = LocalClient::new(StateLayout::in_state_root(&state_root));
        let probe = probe_running_backend(&client, STATUS_PROBE_TIMEOUT).await;
        let message = format_install_restart_failure_diagnostic(&unit_path, &reason, probe);
        return Err(LocalError::new(LocalErrorKind::Internal, &message));
    }

    let client = LocalClient::new(StateLayout::in_state_root(&state_root));
    let expected_daemon = canonical_or_original_path(&daemon_path);
    verify_backend_after_success(
        &client,
        CompletedAction::Install {
            unit_path: &unit_path,
            daemon_path: &expected_daemon,
        },
        STATUS_PROBE_TIMEOUT,
    )
    .await?;

    Ok(unit_path)
}

/// 定位并核验与 `current_exe` 同目录下的 `lattice-daemon`：必须存在、是普通文件且对当前有效用户身份可执行。
fn resolve_sibling_daemon(current_exe: &Path) -> Result<PathBuf, LocalError> {
    let Some(exe_dir) = current_exe.parent() else {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "无法从当前可执行文件路径确定所在目录：{}",
                current_exe.display()
            ),
        ));
    };
    let daemon_path = exe_dir.join("lattice-daemon");
    validate_daemon_binary(&daemon_path)?;
    Ok(daemon_path)
}

fn validate_daemon_binary(daemon_path: &Path) -> Result<(), LocalError> {
    if !daemon_path.is_absolute() {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "同目录下的 `lattice-daemon` 路径不是绝对路径：{}",
                daemon_path.display()
            ),
        ));
    }
    let meta = fs::metadata(daemon_path).map_err(|error| {
        LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "同目录下未找到可用的 `lattice-daemon`（{}）：{error}",
                daemon_path.display()
            ),
        )
    })?;
    if !meta.is_file() {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "同目录下的 `lattice-daemon` 不是普通文件：{}",
                daemon_path.display()
            ),
        ));
    }
    rustix::fs::accessat(
        rustix::fs::CWD,
        daemon_path,
        rustix::fs::Access::EXEC_OK,
        rustix::fs::AtFlags::EACCESS,
    )
    .map_err(|error| {
        LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "同目录下的 `lattice-daemon` 对当前用户没有可执行权限（{}）：{error}",
                daemon_path.display()
            ),
        )
    })?;
    Ok(())
}

/// 按 XDG 规则解析 `lattice.service` 路径：优先取绝对路径的 `XDG_CONFIG_HOME`，否则回退到 `$HOME/.config`。
fn resolve_unit_path_from_env(
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, LocalError> {
    let config_home = match xdg_config_home.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => {
            let home_path = home
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| {
                    LocalError::new(
                        LocalErrorKind::BadRequest,
                        "无法解析配置目录：请设置绝对路径的 XDG_CONFIG_HOME 或 HOME",
                    )
                })?;
            home_path.join(".config")
        }
    };
    Ok(config_home
        .join("systemd")
        .join("user")
        .join("lattice.service"))
}

/// 计算 `TimeoutStopSec`：排空上限加 30 秒，整秒写 `Ns`，否则写 `Nms`；溢出返回 `None`。
pub(super) fn format_timeout_stop_sec(drain_timeout: Duration) -> Option<String> {
    let total = drain_timeout.checked_add(Duration::from_secs(STOP_TIMEOUT_MARGIN_SECS))?;
    format_duration_unit_value(total)
}

fn format_drain_timeout_arg(drain_timeout: Duration) -> Option<String> {
    format_duration_unit_value(drain_timeout)
}

fn format_duration_unit_value(duration: Duration) -> Option<String> {
    if duration.subsec_nanos() == 0 {
        Some(format!("{}s", duration.as_secs()))
    } else {
        let millis = u64::try_from(duration.as_millis()).ok()?;
        Some(format!("{millis}ms"))
    }
}

/// 按 systemd `ExecStart=` 对第一个词（可执行文件路径）的规则格式化：
/// - 必须是非空、以 `/` 开头、长度小于 `PATH_MAX`（4096 字节）的有效 UTF-8 绝对路径；
/// - 拒绝单双引号、反斜杠、制表符及所有控制字符（含换行、回车、NUL、DEL）；
/// - 保留字面的 `$`（systemd 不对可执行路径做环境变量替换），`%` 写为 `%%`；
/// - 含空白字符时将整个路径用双引号包裹。
pub(super) fn escape_systemd_exec_path(raw: &OsStr) -> Result<String, LocalError> {
    const SYSTEMD_PATH_MAX: usize = 4096;

    let Some(text) = raw.to_str() else {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "可执行文件路径包含非 UTF-8 字节，无法写入 systemd unit：{}",
                Path::new(raw).display()
            ),
        ));
    };
    if text.is_empty() || !text.starts_with('/') {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!("可执行文件路径必须是非空绝对路径：{text:?}"),
        ));
    }
    if text.len() >= SYSTEMD_PATH_MAX {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            "可执行文件路径超过 systemd 允许的最大长度",
        ));
    }
    for ch in text.chars() {
        if matches!(ch, '"' | '\'' | '\\' | '\t') {
            return Err(LocalError::new(
                LocalErrorKind::BadRequest,
                &format!("可执行文件路径包含引号、反斜杠或制表符，systemd 不接受该路径：{text:?}"),
            ));
        }
        if ch.is_control() {
            return Err(LocalError::new(
                LocalErrorKind::BadRequest,
                &format!(
                    "可执行文件路径包含换行符、NUL 或控制字符，无法写入 systemd unit：{text:?}"
                ),
            ));
        }
    }

    let needs_quotes = text.chars().any(char::is_whitespace);
    let mut out = String::with_capacity(text.len() + 8);
    if needs_quotes {
        out.push('"');
    }
    for ch in text.chars() {
        match ch {
            '%' => out.push_str("%%"),
            other => out.push(other),
        }
    }
    if needs_quotes {
        out.push('"');
    }
    Ok(out)
}

/// 按 systemd `ExecStart=` 语法转义后续参数（非 shell 语法）：
/// - 拒绝空串、非 UTF-8、换行、回车、NUL 及其他控制字符（`\t` 除外）；
/// - 含空白、单双引号或反斜杠时外加双引号，并将 `\` 写为 `\\`、`"` 写为 `\"`、`\t` 写为 `\t`；
/// - `%` 写为 `%%`，`$` 写为 `$$`，阻止 systemd 展开单元说明符与环境变量。
pub(super) fn escape_systemd_arg(raw: &OsStr) -> Result<String, LocalError> {
    let Some(text) = raw.to_str() else {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "路径包含非 UTF-8 字节，无法写入 systemd unit：{}",
                Path::new(raw).display()
            ),
        ));
    };
    if text.is_empty() {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            "systemd 命令参数不能为空",
        ));
    }
    for ch in text.chars() {
        if matches!(ch, '\0' | '\n' | '\r') || (ch.is_control() && ch != '\t') {
            return Err(LocalError::new(
                LocalErrorKind::BadRequest,
                &format!("路径包含换行符、NUL 或控制字符，无法写入 systemd unit：{text:?}"),
            ));
        }
    }

    let needs_quotes = text
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\'));
    let mut out = String::with_capacity(text.len() + 8);
    if needs_quotes {
        out.push('"');
    }
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            other => out.push(other),
        }
    }
    if needs_quotes {
        out.push('"');
    }
    Ok(out)
}

pub(super) fn render_unit(
    daemon_path: &Path,
    state_root: &Path,
    drain_timeout: Duration,
) -> Result<String, LocalError> {
    let drain_arg = format_drain_timeout_arg(drain_timeout)
        .ok_or_else(|| LocalError::new(LocalErrorKind::BadRequest, "排空时长数值溢出"))?;
    let timeout_stop = format_timeout_stop_sec(drain_timeout).ok_or_else(|| {
        LocalError::new(
            LocalErrorKind::BadRequest,
            "排空时长加上 30 秒停机余量后数值溢出",
        )
    })?;
    let exec_daemon = escape_systemd_exec_path(daemon_path.as_os_str())?;
    let exec_state_root = escape_systemd_arg(state_root.as_os_str())?;

    Ok(format!(
        "[Unit]\n\
StartLimitIntervalSec=120\n\
StartLimitBurst=5\n\
\n\
[Service]\n\
Type=notify\n\
ExecStart={exec_daemon} --state-root {exec_state_root} --drain-timeout {drain_arg}\n\
TimeoutStopSec={timeout_stop}\n\
NotifyAccess=main\n\
Restart=on-failure\n\
RestartSec=5\n\
RestartPreventExitStatus=2 3 4 SIGKILL\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    ))
}

fn write_unit_atomically(unit_path: &Path, contents: &str) -> Result<(), LocalError> {
    let Some(parent) = unit_path.parent() else {
        return Err(LocalError::new(
            LocalErrorKind::Internal,
            &format!("unit 路径缺少父目录：{}", unit_path.display()),
        ));
    };
    fs::create_dir_all(parent).map_err(|error| {
        LocalError::new(
            LocalErrorKind::Internal,
            &format!("创建 unit 目录 {} 失败：{error}", parent.display()),
        )
    })?;

    let pid = std::process::id();
    let (mut file, tmp_path) = (0..64_u32)
        .find_map(|_| {
            let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(".lattice.service.tmp.{pid}.{seq}"));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => Some(Ok((file, candidate))),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => None,
                Err(err) => Some(Err(err)),
            }
        })
        .unwrap_or_else(|| {
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "无法分配唯一的临时 unit 文件名",
            ))
        })
        .map_err(|error| {
            LocalError::new(
                LocalErrorKind::Internal,
                &format!("创建临时 unit 文件失败（{}）：{error}", parent.display()),
            )
        })?;

    let write_result = (|| -> io::Result<()> {
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp_path, unit_path)?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(LocalError::new(
            LocalErrorKind::Internal,
            &format!("写入 unit 文件 {} 失败：{error}", unit_path.display()),
        ));
    }
    Ok(())
}

async fn run_systemctl_output(args: Vec<OsString>) -> Result<std::process::Output, String> {
    tokio::task::spawn_blocking(move || Command::new("systemctl").args(&args).output())
        .await
        .map_err(|_| "等待 `systemctl` 子进程的工作线程异常退出".to_owned())?
        .map_err(|error| format!("无法执行 `systemctl`：{error}"))
}

async fn run_systemctl(args: Vec<OsString>) -> Result<(), String> {
    let output = run_systemctl_output(args).await?;
    if output.status.success() {
        return Ok(());
    }
    Err(format_command_failure(output.status, &output.stderr))
}

async fn is_unit_not_found() -> bool {
    let args = vec![
        "--user".into(),
        "show".into(),
        "-p".into(),
        "LoadState".into(),
        "--value".into(),
        SERVICE_UNIT_NAME.into(),
    ];
    match run_systemctl_output(args).await {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim() == "not-found"
        }
        _ => false,
    }
}

fn format_command_failure(status: ExitStatus, stderr: &[u8]) -> String {
    let status_desc = match status.code() {
        Some(code) => format!("退出码 {code}"),
        None => "被信号终止".to_owned(),
    };
    let detail = String::from_utf8_lossy(stderr);
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        status_desc
    } else {
        let single_line = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{status_desc}：{single_line}")
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BackendProbe {
    Running {
        peer_pid: Option<i32>,
    },
    /// 对端在同一连接上回写了更旧的协议魔数，并带出发请求前捕获的对端事实。
    Older {
        peer_pid: Option<i32>,
        peer_executable: Option<PathBuf>,
        diagnostic: String,
    },
    /// 其余协议不兼容，即对端协议版本比本前端更新。
    Incompatible {
        diagnostic: String,
    },
    Unreachable,
}

async fn probe_running_backend(client: &LocalClient, timeout: Duration) -> BackendProbe {
    match client.probe_status(timeout).await {
        Ok(StatusProbe::Running { peer_pid, .. }) => BackendProbe::Running { peer_pid },
        Ok(StatusProbe::OlderBackend {
            peer_pid,
            peer_executable,
            error,
        }) => BackendProbe::Older {
            peer_pid,
            peer_executable,
            diagnostic: error.message,
        },
        Err(error) if error.kind == LocalErrorKind::IncompatibleBackend => {
            BackendProbe::Incompatible {
                diagnostic: error.message,
            }
        }
        Err(_) => BackendProbe::Unreachable,
    }
}

/// `systemctl` 已成功完成的动作，决定后台协议不兼容时的诊断措辞。
#[derive(Debug, Clone, Copy)]
enum CompletedAction<'a> {
    /// `daemon start`：服务可能原本就在运行。
    Start,
    /// `daemon restart`：`sibling_daemon` 是本前端同目录 `lattice-daemon` 的规范路径，定位不到时为 `None`。
    Restart { sibling_daemon: Option<&'a Path> },
    /// `daemon install`：`daemon_path` 是刚写入 unit 的 `lattice-daemon` 规范路径。
    Install {
        unit_path: &'a Path,
        daemon_path: &'a Path,
    },
}

impl CompletedAction<'_> {
    fn prefix(self) -> String {
        match self {
            Self::Start => "`systemctl --user start lattice.service` 执行成功".into(),
            Self::Restart { .. } => "已重启服务 lattice.service".into(),
            Self::Install { unit_path, .. } => format!(
                "已写入并启用 unit 文件 {}，并已重启服务 lattice.service",
                unit_path.display()
            ),
        }
    }
}

/// 路径可规范化时使用规范路径，否则保留拼出的原路径供诊断展示。
fn canonical_or_original_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 定位本前端同目录的 `lattice-daemon` 诊断路径；当前可执行文件路径或父目录不可得时为 `None`。
fn sibling_daemon_diagnostic_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join("lattice-daemon");
    Some(canonical_or_original_path(&path))
}

/// 核对 `systemctl` 成功后状态根上应答的后台；探测不到后台时不改判已经成功的 systemctl 动作。
async fn verify_backend_after_success(
    client: &LocalClient,
    action: CompletedAction<'_>,
    timeout: Duration,
) -> Result<(), LocalError> {
    match probe_running_backend(client, timeout).await {
        BackendProbe::Running { .. } | BackendProbe::Unreachable => Ok(()),
        BackendProbe::Incompatible { diagnostic } => {
            let message = format!("{}，但{diagnostic}", action.prefix());
            Err(LocalError::new(
                LocalErrorKind::IncompatibleBackend,
                &message,
            ))
        }
        BackendProbe::Older {
            peer_pid,
            peer_executable,
            diagnostic,
        } => {
            let message = format_older_backend_after_success(
                action,
                peer_pid,
                peer_executable.as_deref(),
                &diagnostic,
            );
            Err(LocalError::new(
                LocalErrorKind::IncompatibleBackend,
                &message,
            ))
        }
    }
}

/// 拼装 `systemctl` 成功后仍遇到更旧协议后台的诊断；路径与对端事实只作展示，不改变判定。
fn format_older_backend_after_success(
    action: CompletedAction<'_>,
    peer_pid: Option<i32>,
    peer_executable: Option<&Path>,
    diagnostic: &str,
) -> String {
    let prefix = action.prefix();
    match (action, peer_executable) {
        (CompletedAction::Start, _) => format!("{prefix}，但{diagnostic}"),
        (CompletedAction::Restart { .. } | CompletedAction::Install { .. }, None) => format!(
            "{prefix}，但运行中的后台仍是较旧的不兼容版本（{}）：读不到它的可执行文件路径；请执行 `systemctl --user cat lattice` 核对 `ExecStart` 指向的 `lattice-daemon` 是否与本前端同一批构建",
            format_peer_pid(peer_pid)
        ),
        (
            CompletedAction::Restart {
                sibling_daemon: Some(expected),
            }
            | CompletedAction::Install {
                daemon_path: expected,
                ..
            },
            Some(running),
        ) if expected == running => format!(
            "{prefix}，但运行中的后台仍是较旧的不兼容版本（{}）：实际运行的是本前端同目录的 {}；请把与本前端同一批构建的 `lattice-daemon` 安装到该路径，再执行 `lattice daemon restart`",
            format_peer_pid(peer_pid),
            running.display()
        ),
        (
            CompletedAction::Restart {
                sibling_daemon: Some(expected),
            },
            Some(running),
        ) => format!(
            "{prefix}，但运行中的后台仍是较旧的不兼容版本（{}）：实际运行的是 {}，不是本前端同目录的 {}；请执行 `lattice daemon install` 让服务改用同目录的 `lattice-daemon`",
            format_peer_pid(peer_pid),
            running.display(),
            expected.display()
        ),
        (
            CompletedAction::Restart {
                sibling_daemon: None,
            },
            Some(running),
        ) => format!(
            "{prefix}，但运行中的后台仍是较旧的不兼容版本（{}）：实际运行的是 {}；请执行 `lattice daemon install` 让服务改用同目录的 `lattice-daemon`",
            format_peer_pid(peer_pid),
            running.display()
        ),
        (
            CompletedAction::Install {
                daemon_path: expected,
                ..
            },
            Some(running),
        ) => format!(
            "{prefix}，但运行中的后台仍是较旧的不兼容版本（{}）：实际运行的是 {}，不是刚写入 unit 的 {}；请执行 `systemctl --user cat lattice` 核对生效的 unit 文件与 drop-in",
            format_peer_pid(peer_pid),
            running.display(),
            expected.display()
        ),
    }
}

fn format_peer_pid(peer_pid: Option<i32>) -> String {
    match peer_pid {
        Some(pid) => format!("对端进程号：{pid}"),
        None => "对端进程号不可用".into(),
    }
}

fn format_install_restart_failure_diagnostic(
    unit_path: &Path,
    systemctl_reason: &str,
    probe: BackendProbe,
) -> String {
    match probe {
        BackendProbe::Running { peer_pid } => format!(
            "已写入并启用 unit 文件 {}，但执行 `systemctl --user restart {SERVICE_UNIT_NAME}` 失败（{systemctl_reason}）：状态根上已有后台正在运行（{}）；请先执行 `lattice daemon stop`，再执行 `lattice daemon start`",
            unit_path.display(),
            format_peer_pid(peer_pid)
        ),
        BackendProbe::Older { diagnostic, .. } | BackendProbe::Incompatible { diagnostic } => {
            format!(
                "已写入并启用 unit 文件 {}，但执行 `systemctl --user restart {SERVICE_UNIT_NAME}` 失败（{systemctl_reason}）：{diagnostic}；停止原有后台后再执行 `lattice daemon start`",
                unit_path.display()
            )
        }
        BackendProbe::Unreachable => format!(
            "已写入并启用 unit 文件 {}，但执行 `systemctl --user restart {SERVICE_UNIT_NAME}` 失败（{systemctl_reason}）；请执行 `systemctl --user status lattice` 查看详情",
            unit_path.display()
        ),
    }
}

fn format_service_action_failure_diagnostic(
    action: ServiceAction,
    systemctl_reason: &str,
    probe: BackendProbe,
) -> String {
    let verb = action.systemctl_verb();
    match probe {
        BackendProbe::Running { peer_pid } => format!(
            "执行 `systemctl --user {verb} {SERVICE_UNIT_NAME}` 失败（{systemctl_reason}）：状态根上已有未受 systemd 托管的后台正在运行（{}）；请先执行 `lattice daemon stop`，再执行 `lattice daemon start`",
            format_peer_pid(peer_pid)
        ),
        BackendProbe::Older { diagnostic, .. } | BackendProbe::Incompatible { diagnostic } => {
            format!(
                "执行 `systemctl --user {verb} {SERVICE_UNIT_NAME}` 失败（{systemctl_reason}）：{diagnostic}；停止原有后台后再执行 `lattice daemon start`"
            )
        }
        BackendProbe::Unreachable => format!(
            "执行 `systemctl --user {verb} {SERVICE_UNIT_NAME}` 失败（{systemctl_reason}）；请执行 `systemctl --user status lattice` 查看详情"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener as StdUnixListener;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn timeout_stop_sec_formats_whole_seconds_or_milliseconds_and_catches_overflow() {
        assert_eq!(
            format_timeout_stop_sec(Duration::from_secs(60)).as_deref(),
            Some("90s")
        );
        assert_eq!(
            format_timeout_stop_sec(Duration::from_secs(0)).as_deref(),
            Some("30s")
        );
        assert_eq!(
            format_timeout_stop_sec(Duration::from_millis(300)).as_deref(),
            Some("30300ms")
        );
        assert_eq!(
            format_timeout_stop_sec(Duration::from_millis(1000)).as_deref(),
            Some("31s")
        );
        assert_eq!(
            format_timeout_stop_sec(Duration::from_secs(u64::MAX - 15)),
            None,
            "秒数加 30 秒溢出时必须返回 None"
        );
        assert_eq!(
            format_timeout_stop_sec(Duration::from_millis(u64::MAX - 15)),
            None,
            "毫秒数加 30 秒超出 u64 时必须返回 None"
        );
    }

    #[test]
    fn systemd_exec_path_escaping_preserves_dollar_escapes_percent_quotes_spaces_and_rejects_special_chars()
     {
        assert_eq!(
            escape_systemd_exec_path(OsStr::new("/usr/local/bin/lattice-daemon")).unwrap(),
            "/usr/local/bin/lattice-daemon"
        );
        assert_eq!(
            escape_systemd_exec_path(OsStr::new("/opt/$HOME/100%p/lattice-daemon")).unwrap(),
            "/opt/$HOME/100%%p/lattice-daemon"
        );
        assert_eq!(
            escape_systemd_exec_path(OsStr::new("/opt/my bin/$HOME 100%p/lattice-daemon")).unwrap(),
            r#""/opt/my bin/$HOME 100%%p/lattice-daemon""#
        );

        for bad in [
            OsStr::new(""),
            OsStr::new("relative/lattice-daemon"),
            OsStr::new("/opt/has'single/lattice-daemon"),
            OsStr::new(r#"/opt/has"double/lattice-daemon"#),
            OsStr::new(r"/opt/has\back/lattice-daemon"),
            OsStr::new("/opt/has\ttab/lattice-daemon"),
            OsStr::new("/opt/line\nbreak/lattice-daemon"),
            OsStr::new("/opt/cr\rbreak/lattice-daemon"),
            OsStr::new("/opt/del\x7fchar/lattice-daemon"),
            OsStr::from_bytes(b"/opt/nul\0byte/lattice-daemon"),
            OsStr::from_bytes(b"/opt/non-\xff-utf8/lattice-daemon"),
        ] {
            let err =
                escape_systemd_exec_path(bad).expect_err("systemd 不接受的可执行路径应被拒绝");
            assert_eq!(err.kind, LocalErrorKind::BadRequest, "输入：{bad:?}");
        }
    }

    #[test]
    fn systemd_arg_escaping_handles_quotes_whitespace_tabs_specifiers_env_and_rejects_unrepresentable()
     {
        assert_eq!(
            escape_systemd_arg(OsStr::new("/home/alice/.local/state")).unwrap(),
            "/home/alice/.local/state"
        );
        assert_eq!(
            escape_systemd_arg(OsStr::new("/tmp/100%$HOME/state")).unwrap(),
            "/tmp/100%%$$HOME/state"
        );
        assert_eq!(
            escape_systemd_arg(OsStr::new(
                r#"/tmp/dir with "quote" 'single' \back %p $v/state"#
            ))
            .unwrap(),
            r#""/tmp/dir with \"quote\" 'single' \\back %%p $$v/state""#
        );
        assert_eq!(
            escape_systemd_arg(OsStr::new("/tmp/tab\there/state")).unwrap(),
            r#""/tmp/tab\there/state""#
        );

        for bad in [
            OsStr::new(""),
            OsStr::new("/tmp/line\nbreak"),
            OsStr::new("/tmp/cr\rbreak"),
            OsStr::new("/tmp/del\x7fchar"),
            OsStr::from_bytes(b"/tmp/nul\0byte"),
            OsStr::from_bytes(b"/tmp/non-\xff-utf8"),
        ] {
            let err = escape_systemd_arg(bad).expect_err("不可表示的参数路径应被拒绝");
            assert_eq!(err.kind, LocalErrorKind::BadRequest);
        }
    }

    #[test]
    fn render_unit_applies_distinct_escaping_to_executable_path_and_state_root() {
        let unit = render_unit(
            Path::new("/opt/my bin/$HOME 100%p/lattice-daemon"),
            Path::new("/var/lib/state dir \"quote\" 'single' \\back\ttab %i $USER"),
            Duration::from_millis(300),
        )
        .expect("可执行路径与状态根分别合法时应渲染成功");
        let expected = "\
[Unit]
StartLimitIntervalSec=120
StartLimitBurst=5

[Service]
Type=notify
ExecStart=\"/opt/my bin/$HOME 100%%p/lattice-daemon\" --state-root \"/var/lib/state dir \\\"quote\\\" 'single' \\\\back\\ttab %%i $$USER\" --drain-timeout 300ms
TimeoutStopSec=30300ms
NotifyAccess=main
Restart=on-failure
RestartSec=5
RestartPreventExitStatus=2 3 4 SIGKILL

[Install]
WantedBy=default.target
";
        assert_eq!(unit, expected);

        let err = render_unit(
            Path::new(r"/opt/bad\daemon/lattice-daemon"),
            Path::new("/home/alice/.local/state"),
            Duration::from_secs(60),
        )
        .expect_err("含反斜杠的可执行路径必须在渲染阶段被拒绝");
        assert_eq!(err.kind, LocalErrorKind::BadRequest);
    }

    #[test]
    fn validate_daemon_binary_checks_effective_user_execute_permission() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let daemon_path = dir.path().join("lattice-daemon");
        fs::write(&daemon_path, b"#!/bin/sh\nexit 0\n").unwrap();

        fs::set_permissions(&daemon_path, fs::Permissions::from_mode(0o644)).unwrap();
        let err_644 = validate_daemon_binary(&daemon_path).expect_err("0644 不可执行应被拒绝");
        assert_eq!(err_644.kind, LocalErrorKind::BadRequest);

        // 非 root 用户拥有、权限 0641（仅 other 有 x 位，属主无 x 位）时当前有效身份不可执行。
        if unsafe { libc::geteuid() } != 0 {
            fs::set_permissions(&daemon_path, fs::Permissions::from_mode(0o641)).unwrap();
            let err_641 =
                validate_daemon_binary(&daemon_path).expect_err("属主为当前用户的 0641 应被拒绝");
            assert_eq!(err_641.kind, LocalErrorKind::BadRequest);
        }

        fs::set_permissions(&daemon_path, fs::Permissions::from_mode(0o700)).unwrap();
        validate_daemon_binary(&daemon_path).expect("0700 对属主可执行应通过");
    }

    #[test]
    fn render_unit_matches_adr_0011_section_3_verbatim() {
        let unit = render_unit(
            Path::new("/opt/lattice/bin/lattice-daemon"),
            Path::new("/home/alice/.local/state"),
            Duration::from_secs(60),
        )
        .expect("标准路径渲染应成功");
        let expected = "\
[Unit]
StartLimitIntervalSec=120
StartLimitBurst=5

[Service]
Type=notify
ExecStart=/opt/lattice/bin/lattice-daemon --state-root /home/alice/.local/state --drain-timeout 60s
TimeoutStopSec=90s
NotifyAccess=main
Restart=on-failure
RestartSec=5
RestartPreventExitStatus=2 3 4 SIGKILL

[Install]
WantedBy=default.target
";
        assert_eq!(unit, expected);
    }

    #[test]
    fn install_and_service_action_failure_diagnostics_cover_running_incompatible_and_unreachable_cases()
     {
        let unit_path = Path::new("/tmp/cfg/systemd/user/lattice.service");

        let with_pid = format_install_restart_failure_diagnostic(
            unit_path,
            "退出码 1",
            BackendProbe::Running {
                peer_pid: Some(4242),
            },
        );
        assert!(with_pid.contains("对端进程号：4242"));
        assert!(with_pid.contains("lattice daemon stop"));
        assert!(with_pid.contains("lattice daemon start"));

        let without_pid = format_install_restart_failure_diagnostic(
            unit_path,
            "退出码 1",
            BackendProbe::Running { peer_pid: None },
        );
        assert!(without_pid.contains("对端进程号不可用"));
        assert!(without_pid.contains("lattice daemon stop"));

        let incompatible = format_install_restart_failure_diagnostic(
            unit_path,
            "退出码 1",
            BackendProbe::Incompatible {
                diagnostic: "运行中的后台版本较旧且不兼容（对端进程号：5150）；请执行 `lattice daemon restart`（手动运行时执行 `lattice daemon stop` 或 `kill 5150`）切换至新后台".into(),
            },
        );
        assert!(incompatible.contains("版本较旧且不兼容"));
        assert!(incompatible.contains("对端进程号：5150"));
        assert!(!incompatible.contains("没有后台"));
        assert!(incompatible.contains("lattice daemon start"));

        let older = format_install_restart_failure_diagnostic(
            unit_path,
            "退出码 1",
            BackendProbe::Older {
                peer_pid: Some(5150),
                peer_executable: None,
                diagnostic: "运行中的后台版本较旧且不兼容（对端进程号：5150）；请执行 `lattice daemon restart`（手动运行时执行 `lattice daemon stop` 或 `kill 5150`）切换至新后台".into(),
            },
        );
        assert_eq!(
            older, incompatible,
            "失败路径的更旧后台诊断应与协议不兼容分支一致"
        );

        let unreachable = format_install_restart_failure_diagnostic(
            unit_path,
            "退出码 1",
            BackendProbe::Unreachable,
        );
        assert!(unreachable.contains("systemctl --user status lattice"));

        let start_conflict = format_service_action_failure_diagnostic(
            ServiceAction::Start,
            "退出码 1",
            BackendProbe::Running {
                peer_pid: Some(7788),
            },
        );
        assert!(start_conflict.contains("systemctl --user start lattice.service"));
        assert!(start_conflict.contains("对端进程号：7788"));
        assert!(start_conflict.contains("lattice daemon stop"));

        let restart_unreachable = format_service_action_failure_diagnostic(
            ServiceAction::Restart,
            "退出码 1",
            BackendProbe::Unreachable,
        );
        assert!(restart_unreachable.contains("systemctl --user restart lattice.service"));
        assert!(restart_unreachable.contains("systemctl --user status lattice"));
    }

    #[tokio::test]
    async fn probe_running_backend_preserves_incompatible_version_and_peer_pid() {
        let dir = tempfile::Builder::new()
            .prefix("ltc-inst-")
            .tempdir_in("/tmp")
            .expect("创建短路径临时目录");
        let layout = StateLayout::in_state_root(dir.path());
        let local_dir = layout.lattice_root().join("local");
        fs::create_dir_all(&local_dir).unwrap();
        fs::set_permissions(layout.lattice_root(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let sock_path = local_dir.join("api.sock");
        let std_listener = StdUnixListener::bind(&sock_path).unwrap();
        fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(std_listener).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bad_magic = *lattice_local::test_support::FRAME_MAGIC;
            bad_magic[7] = bad_magic[7].wrapping_sub(1);
            stream.write_all(&bad_magic).await.unwrap();
            let _ = stream.shutdown().await;
        });

        let client = LocalClient::new(layout);
        let probe = probe_running_backend(&client, Duration::from_secs(2)).await;
        server.await.unwrap();

        let expected_pid = format!("对端进程号：{}", std::process::id());
        match probe {
            BackendProbe::Older {
                peer_pid,
                peer_executable,
                diagnostic,
            } => {
                assert_eq!(peer_pid, Some(std::process::id() as i32));
                assert_eq!(
                    peer_executable,
                    Some(std::env::current_exe().expect("读取当前测试进程路径"))
                );
                assert!(
                    diagnostic.contains("版本较旧且不兼容") && diagnostic.contains(&expected_pid),
                    "更旧后台探测应保留版本诊断与对端进程号：{diagnostic}"
                );
            }
            other => panic!("应识别为更旧协议后台，实际：{other:?}"),
        }
    }

    #[test]
    fn success_after_systemctl_diagnostics_lock_restart_and_install_wording() {
        let unit_path = Path::new("/tmp/cfg/systemd/user/lattice.service");
        let same = Path::new("/opt/lattice/bin/lattice-daemon");
        let other = Path::new("/usr/local/bin/lattice-daemon");

        assert_eq!(
            format_older_backend_after_success(
                CompletedAction::Restart {
                    sibling_daemon: Some(same),
                },
                Some(4242),
                Some(same),
                "测试诊断",
            ),
            "已重启服务 lattice.service，但运行中的后台仍是较旧的不兼容版本（对端进程号：4242）：实际运行的是本前端同目录的 /opt/lattice/bin/lattice-daemon；请把与本前端同一批构建的 `lattice-daemon` 安装到该路径，再执行 `lattice daemon restart`"
        );
        assert_eq!(
            format_older_backend_after_success(
                CompletedAction::Restart {
                    sibling_daemon: Some(same),
                },
                Some(4242),
                Some(other),
                "测试诊断",
            ),
            "已重启服务 lattice.service，但运行中的后台仍是较旧的不兼容版本（对端进程号：4242）：实际运行的是 /usr/local/bin/lattice-daemon，不是本前端同目录的 /opt/lattice/bin/lattice-daemon；请执行 `lattice daemon install` 让服务改用同目录的 `lattice-daemon`"
        );
        assert_eq!(
            format_older_backend_after_success(
                CompletedAction::Install {
                    unit_path,
                    daemon_path: same,
                },
                Some(4242),
                Some(other),
                "测试诊断",
            ),
            "已写入并启用 unit 文件 /tmp/cfg/systemd/user/lattice.service，并已重启服务 lattice.service，但运行中的后台仍是较旧的不兼容版本（对端进程号：4242）：实际运行的是 /usr/local/bin/lattice-daemon，不是刚写入 unit 的 /opt/lattice/bin/lattice-daemon；请执行 `systemctl --user cat lattice` 核对生效的 unit 文件与 drop-in"
        );
        assert_eq!(
            format_older_backend_after_success(
                CompletedAction::Restart {
                    sibling_daemon: Some(same),
                },
                None,
                None,
                "测试诊断",
            ),
            "已重启服务 lattice.service，但运行中的后台仍是较旧的不兼容版本（对端进程号不可用）：读不到它的可执行文件路径；请执行 `systemctl --user cat lattice` 核对 `ExecStart` 指向的 `lattice-daemon` 是否与本前端同一批构建"
        );
        assert_eq!(
            format_older_backend_after_success(
                CompletedAction::Start,
                Some(4242),
                Some(same),
                "测试诊断",
            ),
            "`systemctl --user start lattice.service` 执行成功，但测试诊断"
        );
    }

    #[tokio::test]
    async fn success_probe_wiring_reports_older_newer_and_ignores_unreachable_backend() {
        use std::time::{SystemTime, UNIX_EPOCH};

        struct TempDir(PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let make_layout = |tag: &str| {
            let temp = TempDir(PathBuf::from("/tmp").join(format!(
                "ltc-success-probe-{tag}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
            fs::create_dir_all(&temp.0).unwrap();
            let layout = StateLayout::in_state_root(&temp.0);
            let local_dir = layout.lattice_root().join("local");
            fs::create_dir_all(&local_dir).unwrap();
            fs::set_permissions(layout.lattice_root(), fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700)).unwrap();
            let sock_path = local_dir.join("api.sock");
            let listener = StdUnixListener::bind(&sock_path).unwrap();
            fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let listener = tokio::net::UnixListener::from_std(listener).unwrap();
            (temp, layout, listener)
        };
        let spawn_magic_peer = |listener: tokio::net::UnixListener, version_delta: i16| {
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut magic = [0_u8; 8];
                tokio::io::AsyncReadExt::read_exact(&mut stream, &mut magic)
                    .await
                    .unwrap();
                let version =
                    (lattice_local::test_support::FRAME_MAGIC[7] as i16 + version_delta) as u8;
                let mut peer_magic = *lattice_local::test_support::FRAME_MAGIC;
                peer_magic[7] = version;
                stream.write_all(&peer_magic).await.unwrap();
                stream.write_u32(4).await.unwrap();
                stream.write_all(b"null").await.unwrap();
            })
        };

        // 1. restart 后仍是更旧后台：注入的对端与本前端同路径时，指出应安装同一批构建。
        let (_t1, layout, listener) = make_layout("older");
        let server = spawn_magic_peer(listener, -1);
        let current_exe = std::env::current_exe().unwrap();
        let err = verify_backend_after_success(
            &LocalClient::new(layout),
            CompletedAction::Restart {
                sibling_daemon: Some(&current_exe),
            },
            Duration::from_secs(2),
        )
        .await
        .expect_err("restart 后发现更旧后台必须报错");
        assert_eq!(err.kind, LocalErrorKind::IncompatibleBackend);
        assert!(err.message.contains("仍是较旧的不兼容版本"));
        assert!(err.message.contains("实际运行的是本前端同目录的"));
        assert!(err.message.contains(&current_exe.display().to_string()));
        server.await.unwrap();

        // 2. restart 后是更新后台：沿用客户端的“较新”诊断接在成功前缀后。
        let (_t2, layout, listener) = make_layout("newer");
        let server = spawn_magic_peer(listener, 1);
        let err = verify_backend_after_success(
            &LocalClient::new(layout),
            CompletedAction::Restart {
                sibling_daemon: Some(&current_exe),
            },
            Duration::from_secs(2),
        )
        .await
        .expect_err("restart 后发现更新后台必须报错");
        assert!(
            err.message
                .starts_with("已重启服务 lattice.service，但运行中的后台版本较新且不兼容"),
            "诊断：{}",
            err.message
        );
        server.await.unwrap();

        // 3. start 后仍是更旧后台：沿用普通不兼容诊断并提示 restart。
        let (_t3, layout, listener) = make_layout("start-older");
        let server = spawn_magic_peer(listener, -1);
        let err = verify_backend_after_success(
            &LocalClient::new(layout),
            CompletedAction::Start,
            Duration::from_secs(2),
        )
        .await
        .expect_err("start 后发现更旧后台必须报错");
        assert!(
            err.message.starts_with(
                "`systemctl --user start lattice.service` 执行成功，但运行中的后台版本较旧且不兼容"
            ),
            "诊断：{}",
            err.message
        );
        assert!(err.message.contains("lattice daemon restart"));
        server.await.unwrap();

        // 4. 状态根上没有 api.sock：不改判 systemctl 已成功执行的动作。
        let temp = TempDir(PathBuf::from("/tmp").join(format!(
            "ltc-success-probe-empty-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        fs::create_dir_all(&temp.0).unwrap();
        verify_backend_after_success(
            &LocalClient::new(StateLayout::in_state_root(&temp.0)),
            CompletedAction::Restart {
                sibling_daemon: Some(&current_exe),
            },
            Duration::from_millis(100),
        )
        .await
        .expect("探测不到后台时不得改判 systemctl 成功");
    }
}
