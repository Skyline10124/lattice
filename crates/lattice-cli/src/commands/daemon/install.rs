// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice daemon install`：写出 `lattice.service` 并通过 `systemctl --user` 启用与启动后台。

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, ExitStatus},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use clap::{ArgAction, Args};
use lattice_local::{
    DrainTimeout, LocalClient, LocalError, LocalErrorKind, StateLayout, parse_drain_timeout,
    xdg_state_root,
};

/// `enable --now` 失败后探测状态根上是否已有后台的单次截止时间。
const STATUS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// `TimeoutStopSec` 在排空上限之外附加的尾段收尾与停写余量（秒）。
const STOP_TIMEOUT_MARGIN_SECS: u64 = 30;

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
            println!("已启用并启动服务 lattice.service。");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("错误：{}", error.message);
            ExitCode::from(exit_code_for_kind(error.kind))
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

    if let Err(reason) = run_systemctl(&["--user", "daemon-reload"]).await {
        return Err(LocalError::new(
            LocalErrorKind::Internal,
            &format!(
                "已写入 unit 文件 {}，但执行 `systemctl --user daemon-reload` 失败（{reason}）",
                unit_path.display()
            ),
        ));
    }

    if let Err(reason) = run_systemctl(&["--user", "enable", "--now", "lattice"]).await {
        let client = LocalClient::new(StateLayout::in_state_root(&state_root));
        let probe = probe_running_backend(&client, STATUS_PROBE_TIMEOUT).await;
        let message = format_enable_failure_diagnostic(&unit_path, &reason, probe);
        return Err(LocalError::new(LocalErrorKind::Internal, &message));
    }

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

async fn run_systemctl(args: &[&'static str]) -> Result<(), String> {
    let args_owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let output =
        tokio::task::spawn_blocking(move || Command::new("systemctl").args(&args_owned).output())
            .await
            .map_err(|_| "等待 `systemctl` 子进程的工作线程异常退出".to_owned())?
            .map_err(|error| format!("无法执行 `systemctl`：{error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format_command_failure(output.status, &output.stderr))
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
    Running { peer_pid: Option<i32> },
    Incompatible { diagnostic: String },
    Unreachable,
}

async fn probe_running_backend(client: &LocalClient, timeout: Duration) -> BackendProbe {
    match client.status_with_peer_pid(timeout).await {
        Ok((_status, peer_pid)) => BackendProbe::Running { peer_pid },
        Err(error) if error.kind == LocalErrorKind::IncompatibleBackend => {
            BackendProbe::Incompatible {
                diagnostic: error.message,
            }
        }
        Err(_) => BackendProbe::Unreachable,
    }
}

fn format_peer_pid(peer_pid: Option<i32>) -> String {
    match peer_pid {
        Some(pid) => format!("对端进程号：{pid}"),
        None => "对端进程号不可用".into(),
    }
}

fn format_enable_failure_diagnostic(
    unit_path: &Path,
    systemctl_reason: &str,
    probe: BackendProbe,
) -> String {
    match probe {
        BackendProbe::Running { peer_pid } => format!(
            "已写入 unit 文件 {}，但执行 `systemctl --user enable --now lattice` 失败（{systemctl_reason}）：状态根上已有后台正在运行（{}）；请先执行 `lattice daemon stop`，再执行 `systemctl --user start lattice`",
            unit_path.display(),
            format_peer_pid(peer_pid)
        ),
        BackendProbe::Incompatible { diagnostic } => format!(
            "已写入 unit 文件 {}，但执行 `systemctl --user enable --now lattice` 失败（{systemctl_reason}）：{diagnostic}；停止原有后台后再执行 `systemctl --user start lattice`",
            unit_path.display()
        ),
        BackendProbe::Unreachable => format!(
            "已写入 unit 文件 {}，但执行 `systemctl --user enable --now lattice` 失败（{systemctl_reason}）；请执行 `systemctl --user status lattice` 查看详情",
            unit_path.display()
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
    fn enable_failure_diagnostics_cover_running_incompatible_and_unreachable_cases() {
        let unit_path = Path::new("/tmp/cfg/systemd/user/lattice.service");

        let with_pid = format_enable_failure_diagnostic(
            unit_path,
            "退出码 1",
            BackendProbe::Running {
                peer_pid: Some(4242),
            },
        );
        assert!(with_pid.contains("对端进程号：4242"));
        assert!(with_pid.contains("lattice daemon stop"));
        assert!(with_pid.contains("systemctl --user start lattice"));

        let without_pid = format_enable_failure_diagnostic(
            unit_path,
            "退出码 1",
            BackendProbe::Running { peer_pid: None },
        );
        assert!(without_pid.contains("对端进程号不可用"));
        assert!(without_pid.contains("lattice daemon stop"));

        let incompatible = format_enable_failure_diagnostic(
            unit_path,
            "退出码 1",
            BackendProbe::Incompatible {
                diagnostic: "运行中的后台版本较旧且不兼容（对端进程号：5150）；请用旧二进制执行 `lattice daemon stop` 停止它".into(),
            },
        );
        assert!(incompatible.contains("版本较旧且不兼容"));
        assert!(incompatible.contains("对端进程号：5150"));
        assert!(!incompatible.contains("没有后台"));
        assert!(incompatible.contains("systemctl --user start lattice"));

        let unreachable =
            format_enable_failure_diagnostic(unit_path, "退出码 1", BackendProbe::Unreachable);
        assert!(unreachable.contains("systemctl --user status lattice"));
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
            BackendProbe::Incompatible { diagnostic } => {
                assert!(
                    diagnostic.contains("版本较旧且不兼容") && diagnostic.contains(&expected_pid),
                    "不兼容后台探测应保留版本诊断与对端进程号：{diagnostic}"
                );
            }
            other => panic!("应识别为协议不兼容后台，实际：{other:?}"),
        }
    }
}
