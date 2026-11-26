// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 统一管理 `lattice` 命令树的中文帮助模板、选项排序、值解析与参数错误格式化。

use std::{error::Error as _, ffi::OsString};

use clap::error::{ContextKind, ContextValue, ErrorKind};

const TEMPLATE_POS_SUB_OPT: &str = "\
{about-with-newline}
用法：{usage}

参数：
{positionals}

命令：
{subcommands}

选项：
{options}";

const TEMPLATE_POS_OPT: &str = "\
{about-with-newline}
用法：{usage}

参数：
{positionals}

选项：
{options}";

const TEMPLATE_SUB_OPT: &str = "\
{about-with-newline}
用法：{usage}

命令：
{subcommands}

选项：
{options}";

const TEMPLATE_OPT: &str = "\
{about-with-newline}
用法：{usage}

选项：
{options}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonSupport {
    RootOrSession,
    StreamJsonl,
    TurnFollow,
    SingleJson,
    Unsupported,
}

/// 命令行参数解析错误（含不支持 `--json` 与 `models refresh` 过滤选项冲突）。
#[derive(Debug)]
pub(crate) enum CliParseError {
    Clap(clap::Error),
    JsonUnsupported(&'static str),
    ModelsRefreshFilterConflict,
}

impl From<clap::Error> for CliParseError {
    fn from(error: clap::Error) -> Self {
        Self::Clap(error)
    }
}

/// 对整个 `lattice` 命令树统一应用中文帮助模板、用法行、选项顺序与全局选项说明。
pub(crate) fn localize_cli(mut cmd: clap::Command) -> clap::Command {
    cmd = disable_help_subcommands(cmd);
    cmd.build();
    localize_recursive(cmd, &[])
}

fn disable_help_subcommands(cmd: clap::Command) -> clap::Command {
    let sub_names: Vec<String> = cmd
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    let mut cmd = cmd.disable_help_subcommand(true);
    for name in sub_names {
        cmd = cmd.mut_subcommand(&name, disable_help_subcommands);
    }
    cmd
}

fn localize_recursive(mut cmd: clap::Command, path: &[&str]) -> clap::Command {
    let has_positionals = cmd.get_positionals().any(|arg| !arg.is_hide_set());
    let has_subcommands = cmd.get_subcommands().any(|sub| !sub.is_hide_set());
    let template = match (has_positionals, has_subcommands) {
        (true, true) => TEMPLATE_POS_SUB_OPT,
        (true, false) => TEMPLATE_POS_OPT,
        (false, true) => TEMPLATE_SUB_OPT,
        (false, false) => TEMPLATE_OPT,
    };

    cmd = cmd.help_template(template);
    if let Some(usage) = usage_for_path(path) {
        cmd = cmd.override_usage(usage);
    }

    let json_support = json_support_for_path(path);
    if cmd.get_arguments().any(|arg| arg.get_id() == "json") {
        cmd = cmd.mut_arg("json", |arg| match json_support {
            JsonSupport::Unsupported => arg.hide(true),
            JsonSupport::RootOrSession => arg
                .help("以 JSON 或 JSONL 格式输出结果")
                .long_help(None)
                .display_order(1000),
            JsonSupport::StreamJsonl => arg
                .help("以 JSONL 流式输出本轮记录")
                .long_help(None)
                .display_order(1000),
            JsonSupport::TurnFollow => arg
                .help("以 JSON 输出结果，配合 --follow 时输出 JSONL 流")
                .long_help(None)
                .display_order(1000),
            JsonSupport::SingleJson => arg
                .help("以 JSON 输出结果")
                .long_help(None)
                .display_order(1000),
        });
    }
    if cmd.get_arguments().any(|arg| arg.get_id() == "verbose") {
        cmd = cmd.mut_arg("verbose", |arg| {
            arg.help("输出详细日志").long_help(None).display_order(1001)
        });
    }
    if cmd.get_arguments().any(|arg| arg.get_id() == "help") {
        cmd = cmd.mut_arg("help", |arg| {
            arg.action(clap::ArgAction::HelpShort)
                .help("显示帮助")
                .long_help(None)
                .display_order(1002)
        });
    }
    if cmd.get_arguments().any(|arg| arg.get_id() == "version") {
        cmd = cmd.mut_arg("version", |arg| {
            arg.help("显示版本信息").long_help(None).display_order(1003)
        });
    }

    let sub_names: Vec<String> = cmd
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    for name in sub_names {
        let mut child_path = path.to_vec();
        child_path.push(name.as_str());
        cmd = cmd.mut_subcommand(&name, |sub| localize_recursive(sub, &child_path));
    }
    cmd
}

fn json_support_for_path(path: &[&str]) -> JsonSupport {
    match path {
        [] | ["session"] => JsonSupport::RootOrSession,
        ["run"] | ["session", "send"] | ["session", "trigger"] => JsonSupport::StreamJsonl,
        ["session", "turn"] => JsonSupport::TurnFollow,
        ["daemon", "install"] | ["tui"] => JsonSupport::Unsupported,
        _ => JsonSupport::SingleJson,
    }
}

fn usage_for_path(path: &[&str]) -> Option<&'static str> {
    Some(match path {
        [] => "lattice [选项] [命令]",
        ["run"] => "lattice run [选项] [消息]",
        ["daemon"] => "lattice daemon [选项] <命令>",
        ["daemon", "status"] => "lattice daemon status [选项]",
        ["daemon", "stop"] => "lattice daemon stop [选项]",
        ["daemon", "install"] => "lattice daemon install [选项]",
        ["im"] => "lattice im [选项] <命令>",
        ["im", "auth"] => "lattice im auth [选项] <命令>",
        ["im", "auth", "set"] => "lattice im auth set [选项] <平台>",
        ["im", "auth", "remove"] => "lattice im auth remove [选项] <平台>",
        ["im", "auth", "status"] => "lattice im auth status [选项]",
        ["im", "reload"] => "lattice im reload [选项]",
        ["auth"] => "lattice auth [选项] <命令>",
        ["auth", "set"] => "lattice auth set [选项] <provider>",
        ["auth", "login"] => "lattice auth login [选项] <provider>",
        ["auth", "logout"] => "lattice auth logout [选项] <provider>",
        ["auth", "remove"] => "lattice auth remove [选项] <provider>",
        ["auth", "status"] => "lattice auth status [选项]",
        ["models"] => "lattice models [选项] [命令]",
        ["models", "refresh"] => "lattice models refresh [选项]",
        ["session"] => "lattice session [选项] <命令>",
        ["session", "send"] => "lattice session send [选项] [消息]",
        ["session", "feed"] => "lattice session feed [选项] <会话ID> [消息]",
        ["session", "context-op"] => "lattice session context-op [选项] <会话ID>",
        ["session", "trigger"] => "lattice session trigger [选项] <会话ID>",
        ["session", "inspect"] => "lattice session inspect [选项] <会话ID>",
        ["session", "rewind"] => "lattice session rewind [选项] <会话ID> <节点号>",
        ["session", "list"] => "lattice session list [选项]",
        ["session", "log"] => "lattice session log [选项] <会话ID>",
        ["session", "usage"] => "lattice session usage [选项] <会话ID>",
        ["session", "turn"] => "lattice session turn [选项] <轮次ID>",
        ["session", "cancel"] => "lattice session cancel [选项] <会话ID>",
        ["tui"] => "lattice tui [选项] [会话ID]",
        _ => return None,
    })
}

/// 生成不支持 `--json` 命令的中文错误文案。
pub(crate) fn json_unsupported_message(command: &str) -> String {
    format!("`lattice {command}` 不支持 `--json`；执行 `lattice {command} --help` 查看用法")
}

/// 按命令行参数前缀定位最深层已知子命令对应的 `--help` 提示。
fn help_hint_for_args(args: &[OsString]) -> &'static str {
    let mut words = Vec::new();
    let mut iter = args
        .iter()
        .skip(1)
        .take_while(|arg| arg.as_os_str() != "--");
    while let Some(arg) = iter.next() {
        let skip = crate::option_value_arity(arg.as_os_str());
        if skip > 0 {
            for _ in 0..skip {
                let _ = iter.next();
            }
            continue;
        }
        if arg.as_encoded_bytes().starts_with(b"-") || arg.as_os_str() == "help" {
            continue;
        }
        if let Some(word) = arg.to_str() {
            words.push(word);
            if words.len() == 3 {
                break;
            }
        } else {
            break;
        }
    }

    match words.as_slice() {
        ["run", ..] => "lattice run --help",
        ["daemon", "status", ..] => "lattice daemon status --help",
        ["daemon", "stop", ..] => "lattice daemon stop --help",
        ["daemon", "install", ..] => "lattice daemon install --help",
        ["daemon", ..] => "lattice daemon --help",
        ["im", "auth", "set", ..] => "lattice im auth set --help",
        ["im", "auth", "remove", ..] => "lattice im auth remove --help",
        ["im", "auth", "status", ..] => "lattice im auth status --help",
        ["im", "auth", ..] => "lattice im auth --help",
        ["im", "reload", ..] => "lattice im reload --help",
        ["im", ..] => "lattice im --help",
        ["auth", "set", ..] => "lattice auth set --help",
        ["auth", "login", ..] => "lattice auth login --help",
        ["auth", "logout", ..] => "lattice auth logout --help",
        ["auth", "remove", ..] => "lattice auth remove --help",
        ["auth", "status", ..] => "lattice auth status --help",
        ["auth", ..] => "lattice auth --help",
        ["models", "refresh", ..] => "lattice models refresh --help",
        ["models", ..] => "lattice models --help",
        ["session", "send", ..] => "lattice session send --help",
        ["session", "feed", ..] => "lattice session feed --help",
        ["session", "context-op", ..] => "lattice session context-op --help",
        ["session", "trigger", ..] => "lattice session trigger --help",
        ["session", "inspect", ..] => "lattice session inspect --help",
        ["session", "rewind", ..] => "lattice session rewind --help",
        ["session", "list", ..] => "lattice session list --help",
        ["session", "log", ..] => "lattice session log --help",
        ["session", "usage", ..] => "lattice session usage --help",
        ["session", "turn", ..] => "lattice session turn --help",
        ["session", "cancel", ..] => "lattice session cancel --help",
        ["session", ..] => "lattice session --help",
        ["tui", ..] => "lattice tui --help",
        _ => "lattice --help",
    }
}

/// 将所有命令行参数解析错误统一格式化为已脱敏的中文正文（不含 `错误：` 前缀）。
pub(crate) fn cli_error_message(args: &[OsString], parse_error: &CliParseError) -> String {
    match parse_error {
        CliParseError::JsonUnsupported(command) => json_unsupported_message(command),
        CliParseError::ModelsRefreshFilterConflict => {
            "`--auth` 和 `--provider` 不能与 `models refresh` 同时使用；执行 `lattice models --help` 查看用法"
                .to_owned()
        }
        CliParseError::Clap(error) => {
            let help_cmd = help_hint_for_args(args);
            match error.kind() {
                ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                | ErrorKind::MissingSubcommand => {
                    format!("缺少子命令；执行 `{help_cmd}` 查看用法")
                }
                ErrorKind::InvalidSubcommand => {
                    format!("未知子命令；执行 `{help_cmd}` 查看用法")
                }
                ErrorKind::MissingRequiredArgument => {
                    if let Some(missing) = format_missing_required_args(error) {
                        format!("缺少必填参数：{missing}；执行 `{help_cmd}` 查看用法")
                    } else {
                        format!("缺少必填参数；执行 `{help_cmd}` 查看用法")
                    }
                }
                ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
                    if let Some(reason) = safe_validation_reason(error) {
                        format!("{reason}；执行 `{help_cmd}` 查看用法")
                    } else {
                        format!("参数值无效；执行 `{help_cmd}` 查看用法")
                    }
                }
                ErrorKind::ArgumentConflict => {
                    format!("参数组合冲突；执行 `{help_cmd}` 查看用法")
                }
                ErrorKind::UnknownArgument => {
                    format!("包含未知参数；执行 `{help_cmd}` 查看用法")
                }
                ErrorKind::TooManyValues
                | ErrorKind::TooFewValues
                | ErrorKind::WrongNumberOfValues => {
                    format!("参数数量不正确；执行 `{help_cmd}` 查看用法")
                }
                _ => format!("命令行参数无效；执行 `{help_cmd}` 查看用法"),
            }
        }
    }
}

fn format_missing_required_args(error: &clap::Error) -> Option<String> {
    let value = error.get(ContextKind::InvalidArg)?;
    let raw: Vec<String> = match value {
        ContextValue::Strings(items) if !items.is_empty() => {
            items.iter().map(|item| item.replace('|', " 或 ")).collect()
        }
        ContextValue::String(item) if !item.is_empty() => {
            vec![item.replace('|', " 或 ")]
        }
        _ => return None,
    };
    Some(raw.join("、"))
}

/// 仅放行由本 crate 或 `lattice_local` 明确生成的安全中文校验消息，杜绝原始输入回显。
fn safe_validation_reason(error: &clap::Error) -> Option<String> {
    let msg = error.source()?.to_string();
    const SAFE_PREFIXES: &[&str] = &[
        "排空时长",
        "Agent 名称",
        "provider 标识",
        "模型引用",
        "推理档位",
        "来源渠道",
        "节点号",
        "轮次 ID",
        "调用 ID",
        "不支持的登录 provider",
    ];
    if SAFE_PREFIXES.iter().any(|prefix| msg.starts_with(prefix)) {
        Some(msg)
    } else {
        None
    }
}

pub(crate) fn parse_agent_name(value: &str) -> Result<lattice_local::AgentName, String> {
    lattice_local::AgentName::parse(value).map_err(|_| "Agent 名称格式无效".to_owned())
}

pub(crate) fn parse_provider_id(value: &str) -> Result<lattice_local::ProviderId, String> {
    lattice_local::ProviderId::parse(value).map_err(|_| "provider 标识格式无效".to_owned())
}

pub(crate) fn parse_model_ref(value: &str) -> Result<lattice_local::ModelRef, String> {
    lattice_local::ModelRef::parse(value)
        .map_err(|_| "模型引用必须为 <provider>/<model> 格式".to_owned())
}

pub(crate) fn parse_effort(value: &str) -> Result<lattice_local::Effort, String> {
    value
        .parse::<lattice_local::Effort>()
        .map_err(|_| "推理档位无效，可选值：off、minimal、low、medium、high、xhigh、max".to_owned())
}

pub(crate) fn parse_channel(value: &str) -> Result<lattice_local::Channel, String> {
    lattice_local::Channel::parse(value).map_err(|_| "来源渠道标签格式无效".to_owned())
}

pub(crate) fn parse_node_id(value: &str) -> Result<lattice_local::NodeId, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("节点号必须为十进制非负整数".to_owned());
    }
    value
        .parse::<u64>()
        .map(lattice_local::NodeId)
        .map_err(|_| "节点号超出 u64 范围".to_owned())
}

pub(crate) fn parse_turn_id(value: &str) -> Result<lattice_local::TurnId, String> {
    lattice_local::TurnId::parse(value).map_err(|_| "轮次 ID 必须为标准 UUID 格式".to_owned())
}

pub(crate) fn parse_invocation_id(value: &str) -> Result<lattice_local::InvocationId, String> {
    lattice_local::InvocationId::parse(value).map_err(|_| "调用 ID 必须为标准 UUID 格式".to_owned())
}

pub(crate) fn parse_login_provider(value: &str) -> Result<String, String> {
    if value == "chatgpt" {
        Ok(value.to_owned())
    } else {
        Err("不支持的登录 provider，当前仅支持 chatgpt".to_owned())
    }
}
