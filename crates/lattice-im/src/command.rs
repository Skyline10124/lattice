// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 发送者白名单与私聊过滤、命令识别以及不落账只读/取消命令执行。

use std::collections::BTreeSet;

use lattice_ipc::{ConflictReason, LocalError, LocalErrorKind, SessionId, StoreDiagnostic, TurnId};
use lattice_server::Service;

use crate::{
    identity::SenderKey,
    redact::{ImRedactor, MAX_DIAGNOSTIC_CHARS, truncate_diagnostic_chars},
    usage,
};

/// 不受支持的媒体或空消息（如贴纸、转码动图、视频、语音、音频、非图片文档等）的固定中文提示。
pub(crate) const UNSUPPORTED_MEDIA_REPLY: &str = "只支持文本和图片消息（PNG、JPEG、WebP、GIF）";

/// `/help` 与 `/start` 共用的固定六行中文命令说明。
pub(crate) const HELP_REPLY: &str = "\
/new — 清除当前会话绑定，下一条消息新建会话
/cancel — 取消当前绑定会话正在执行的轮次
/status — 查看当前绑定会话的状态与设置
/usage — 查看当前绑定会话的用量与费用摘要
/help — 显示本命令说明
直接发送文字或图片（可附说明文字）即向当前会话提交一轮对话。";

/// IM 网关识别的内置斜杠命令。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImCommand {
    New,
    Cancel,
    Status,
    Usage,
    Help,
    Start,
}

/// 授权私聊文本消息的分类结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParsedInput {
    /// 命中内置命令表中的命令。
    Command(ImCommand),
    /// 普通对话消息（包括表外以 `/` 开头的文本，原样交给模型）。
    Message,
}

/// 判定消息是否来自白名单内的私聊发送者。
///
/// 白名单为空时拒绝全部发送者；非私聊或缺少发送者身份的消息一律拒绝。
pub(crate) fn is_authorized_private_message(
    is_private_chat: bool,
    sender: Option<&SenderKey>,
    allowed_senders: &BTreeSet<SenderKey>,
) -> bool {
    if !is_private_chat {
        return false;
    }
    let Some(sender) = sender else {
        return false;
    };
    allowed_senders.contains(sender)
}

/// 识别文本是否命中内置命令表。
///
/// `/start` 允许后跟空白分隔的深链接参数并忽略参数；其余命令仅在整条文本精确匹配时生效，
/// 表外所有文本（如 `/unknown`、`/model`、`/new extra`）均按普通消息提交给模型。
pub(crate) fn parse_text_input(text: &str) -> ParsedInput {
    match text {
        "/new" => ParsedInput::Command(ImCommand::New),
        "/cancel" => ParsedInput::Command(ImCommand::Cancel),
        "/status" => ParsedInput::Command(ImCommand::Status),
        "/usage" => ParsedInput::Command(ImCommand::Usage),
        "/help" => ParsedInput::Command(ImCommand::Help),
        "/start" => ParsedInput::Command(ImCommand::Start),
        _ => {
            if let Some(rest) = text.strip_prefix("/start")
                && rest.chars().next().is_some_and(char::is_whitespace)
            {
                return ParsedInput::Command(ImCommand::Start);
            }
            ParsedInput::Message
        }
    }
}

/// 判定后台错误是否表示目标会话或轮次在持久库中不存在（含会话库尚未首次初始化）。
pub(crate) fn is_not_found_or_uninitialized(error: &LocalError) -> bool {
    error.kind == LocalErrorKind::NotFound
        || matches!(
            error.store.as_deref(),
            Some(StoreDiagnostic::NotInitialized { .. })
        )
}

const UNSUPPORTED_IMAGE_PREFIX: &str = "模型 ";
const UNSUPPORTED_IMAGE_MARKER: &str = " 不支持图片输入";
const CLI_UNSUPPORTED_IMAGE_SUFFIX: &str =
    "；请改用支持图片的模型，或执行 lattice session rewind 退回不含图片的节点";
const LOCAL_BUDGET_PREFIX: &str = "编码后的模型请求超过 LATTICE 本地请求体预算（32 MiB）";
const CLI_REQUEST_SIZE_SUFFIX: &str =
    "，请减少图片、执行 lattice session rewind 退回较早节点或新建会话";
const HTTP_413_STATUS_PREFIX: &str = "HTTP 413";
const HTTP_413_REASON_MARKER: &str = "；目标服务拒绝了本次请求大小（HTTP 413）";
const IM_IMAGE_REJECTION_GUIDANCE: &str =
    "请减少图片、发送 /new 开启新会话，或在终端执行 lattice session rewind 退回较早节点";

/// 按后台固定前缀与后缀精确识别三类图片或请求体大小拒绝（模型不支持图片输入、超过 32 MiB 本地请求体预算、远端 HTTP 413），
/// 在截断前完成脱敏与结构匹配，仅对过长的模型名或 HTTP 413 错误头截断，完整保留中文原因与面向 IM 用户的操作引导。
pub(crate) fn format_backend_image_rejection(
    raw_message: &str,
    redactor: &ImRedactor,
) -> Option<String> {
    let full = redactor.redact_single_line_unbounded(raw_message);

    // 1. 模型不支持图片输入：以 "模型 " 开头，剥离可选 CLI 引导后缀后以 " 不支持图片输入" 结尾。
    let without_unsupported_suffix = full
        .strip_suffix(CLI_UNSUPPORTED_IMAGE_SUFFIX)
        .unwrap_or(&full);
    if let Some(rest) = without_unsupported_suffix.strip_prefix(UNSUPPORTED_IMAGE_PREFIX)
        && let Some(model_name) = rest.strip_suffix(UNSUPPORTED_IMAGE_MARKER)
        && !model_name.is_empty()
    {
        let reserved_chars = UNSUPPORTED_IMAGE_PREFIX.chars().count()
            + UNSUPPORTED_IMAGE_MARKER.chars().count()
            + 1
            + IM_IMAGE_REJECTION_GUIDANCE.chars().count();
        let model_budget = MAX_DIAGNOSTIC_CHARS.saturating_sub(reserved_chars);
        let display_model = truncate_diagnostic_chars(model_name, model_budget);
        return Some(format!(
            "{UNSUPPORTED_IMAGE_PREFIX}{display_model}{UNSUPPORTED_IMAGE_MARKER}；{IM_IMAGE_REJECTION_GUIDANCE}"
        ));
    }

    // 2. 32 MiB 本地请求体预算：精确匹配固定前缀或固定前缀拼接 CLI 请求大小引导后缀。
    if full == LOCAL_BUDGET_PREFIX
        || full
            .strip_prefix(LOCAL_BUDGET_PREFIX)
            .is_some_and(|tail| tail == CLI_REQUEST_SIZE_SUFFIX)
    {
        return Some(format!(
            "{LOCAL_BUDGET_PREFIX}；{IM_IMAGE_REJECTION_GUIDANCE}"
        ));
    }

    // 3. 远端 HTTP 413：以 "HTTP 413" 开头且包含后台 status_error 生成的固定 413 说明后缀。
    if full.starts_with(HTTP_413_STATUS_PREFIX)
        && let Some((head, tail)) = full.split_once(HTTP_413_REASON_MARKER)
        && (tail.is_empty() || tail == CLI_REQUEST_SIZE_SUFFIX)
    {
        let reserved_chars = HTTP_413_REASON_MARKER.chars().count()
            + 1
            + IM_IMAGE_REJECTION_GUIDANCE.chars().count();
        let head_budget = MAX_DIAGNOSTIC_CHARS.saturating_sub(reserved_chars);
        let display_head = truncate_diagnostic_chars(head, head_budget);
        return Some(format!(
            "{display_head}{HTTP_413_REASON_MARKER}；{IM_IMAGE_REJECTION_GUIDANCE}"
        ));
    }

    None
}

/// 将后台 `LocalError` 转换为面向 IM 用户的脱敏单行中文说明。
pub(crate) fn describe_local_error(error: &LocalError, redactor: &ImRedactor) -> String {
    match error.kind {
        LocalErrorKind::Busy => "正在忙，可发 /cancel".into(),
        LocalErrorKind::NotFound => "目标会话或轮次不存在".into(),
        LocalErrorKind::Stopping | LocalErrorKind::NotRunning => "后台正在停止或未运行".into(),
        LocalErrorKind::StoreUnavailable => "会话库暂不可用".into(),
        LocalErrorKind::Internal
        | LocalErrorKind::Transport
        | LocalErrorKind::Protocol
        | LocalErrorKind::Permission
        | LocalErrorKind::AlreadyRunning
        | LocalErrorKind::IncompatibleBackend => "后台内部操作失败".into(),
        LocalErrorKind::Conflict => match error.conflict {
            Some(ConflictReason::TurnIdUsed) => "请求轮次身份冲突".into(),
            Some(ConflictReason::SessionExists) => "会话身份已存在".into(),
            Some(ConflictReason::BindingMismatch) => "会话绑定的工作区与当前配置不一致".into(),
            Some(ConflictReason::SettingsChanged) => "会话设置已变更，请重试".into(),
            Some(ConflictReason::PositionChanged) => "会话位置已变化，请重试".into(),
            None => "请求存在冲突".into(),
        },
        LocalErrorKind::BadRequest => {
            if let Some(guided) = format_backend_image_rejection(&error.message, redactor) {
                return guided;
            }
            let detail = redactor.sanitize_single_line(&error.message);
            if detail.is_empty() || detail == "request or frozen configuration is invalid" {
                "请求或会话配置无效".into()
            } else {
                format!("请求或配置无效（{detail}）")
            }
        }
    }
}

/// 对已知 `TurnId` 执行取消并按统一规则生成中文回复（`/cancel` 与草稿停止按钮共用）。
pub(crate) async fn cancel_turn_and_format(service: &Service, turn_id: TurnId) -> &'static str {
    match service.cancel(turn_id).await {
        Ok(true) => "已取消，等待这一轮结束",
        Ok(false) => "这一轮已结束或不在执行中",
        Err(_) => "取消结果未知，可发 /status 查看",
    }
}

/// 按当前绑定会话的会话头执行 `/cancel` 命令。
pub(crate) async fn execute_cancel_command(
    service: &Service,
    bound_session: Option<SessionId>,
) -> &'static str {
    let Some(session_id) = bound_session else {
        return "没有进行中的轮";
    };
    match service.head(session_id).await {
        Ok(head) => {
            if head.owner
                && let Some(turn_id) = head.turn_id
            {
                cancel_turn_and_format(service, turn_id).await
            } else {
                "没有进行中的轮"
            }
        }
        Err(error) if is_not_found_or_uninitialized(&error) => "没有进行中的轮",
        Err(_) => "取消结果未知，可发 /status 查看",
    }
}

/// 执行 `/status` 命令并生成脱敏后的中文会话状态回复。
pub(crate) async fn execute_status_command(
    service: &Service,
    bound_session: Option<SessionId>,
    redactor: &ImRedactor,
) -> String {
    let Some(session_id) = bound_session else {
        return "当前没有绑定的会话".into();
    };
    match service.head(session_id).await {
        Ok(head) => {
            let execution = if head.owner {
                match head.turn_id {
                    Some(turn_id) => format!("执行中（轮次 {turn_id}）"),
                    None => "执行中".into(),
                }
            } else {
                "空闲".into()
            };
            let model_display = match (&head.provider, &head.model) {
                (Some(provider), Some(model)) => format!("{provider}/{model}"),
                (None, Some(model)) => model.to_string(),
                _ => "未指定（沿用 profile）".into(),
            };
            let effort_display = match head.effort {
                Some(effort) => effort.to_string(),
                None => "默认".into(),
            };
            format!(
                "会话：{}\n状态：{execution}\nAgent：{}\n模型：{model_display}\n推理档位：{effort_display}",
                head.session_id, head.agent
            )
        }
        Err(error) if is_not_found_or_uninitialized(&error) => "当前没有绑定的会话".into(),
        Err(error) => format!(
            "查询会话状态失败：{}",
            describe_local_error(&error, redactor)
        ),
    }
}

/// 执行 `/usage` 命令并生成四行中文用量摘要或状态说明。
pub(crate) async fn execute_usage_command(
    service: &Service,
    bound_session: Option<SessionId>,
    redactor: &ImRedactor,
) -> String {
    let Some(session_id) = bound_session else {
        return "当前没有绑定的会话".into();
    };
    match service.usage(session_id).await {
        Ok(session_usage) => usage::format_session_usage(&session_usage),
        Err(error) if is_not_found_or_uninitialized(&error) => "当前没有绑定的会话".into(),
        Err(error) => format!(
            "查询会话用量失败：{}",
            describe_local_error(&error, redactor)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_by_private_chat_and_sender_allowlist() {
        let allowed_user = SenderKey::from_platform_id("tg", "100");
        let stranger = SenderKey::from_platform_id("tg", "200");

        let empty_allowlist = BTreeSet::new();
        assert!(
            !is_authorized_private_message(true, Some(&allowed_user), &empty_allowlist),
            "空白名单必须拒绝全部发送者"
        );

        let mut allowlist = BTreeSet::new();
        allowlist.insert(allowed_user.clone());

        assert!(is_authorized_private_message(
            true,
            Some(&allowed_user),
            &allowlist
        ));
        assert!(
            !is_authorized_private_message(false, Some(&allowed_user), &allowlist),
            "群组或频道等非私聊消息必须被拒绝"
        );
        assert!(
            !is_authorized_private_message(true, Some(&stranger), &allowlist),
            "不在白名单内的私聊发送者必须被拒绝"
        );
        assert!(
            !is_authorized_private_message(true, None, &allowlist),
            "缺少发送者字段的消息必须被拒绝"
        );
    }

    #[test]
    fn parses_exact_commands_and_treats_unknown_slashes_as_messages() {
        assert_eq!(
            parse_text_input("/new"),
            ParsedInput::Command(ImCommand::New)
        );
        assert_eq!(
            parse_text_input("/cancel"),
            ParsedInput::Command(ImCommand::Cancel)
        );
        assert_eq!(
            parse_text_input("/status"),
            ParsedInput::Command(ImCommand::Status)
        );
        assert_eq!(
            parse_text_input("/usage"),
            ParsedInput::Command(ImCommand::Usage)
        );
        assert_eq!(
            parse_text_input("/help"),
            ParsedInput::Command(ImCommand::Help)
        );
        assert_eq!(
            parse_text_input("/start"),
            ParsedInput::Command(ImCommand::Start)
        );
        assert_eq!(
            parse_text_input("/start deep_link_payload"),
            ParsedInput::Command(ImCommand::Start)
        );
        assert_eq!(
            parse_text_input("/start\t123"),
            ParsedInput::Command(ImCommand::Start)
        );

        for ordinary in [
            "你好",
            "/foo",
            "/model gpt-5",
            "/starter",
            "/new extra",
            "/cancel now",
            " /new",
        ] {
            assert_eq!(
                parse_text_input(ordinary),
                ParsedInput::Message,
                "表外文本必须作为普通消息处理: {ordinary}"
            );
        }

        assert_eq!(
            HELP_REPLY.lines().count(),
            6,
            "/help 与 /start 回复应包含 5 条命令加 1 条直接发送说明"
        );
    }

    #[test]
    fn formats_backend_image_and_size_rejections_with_im_guidance_without_affecting_ordinary_errors()
     {
        let secret = lattice_server::ImToken::parse("bot-secret-token-415").unwrap();
        let redactor = ImRedactor::new([secret]);

        let unsupported_raw = "模型 local/text-only-bot-secret-token-415 不支持图片输入；请改用支持图片的模型，或执行 lattice session rewind 退回不含图片的节点";
        let budget_raw = "编码后的模型请求超过 LATTICE 本地请求体预算（32 MiB），请减少图片、执行 lattice session rewind 退回较早节点或新建会话";
        let http_413_raw = "HTTP 413 INVALID_ARGUMENT: payload bot-secret-token-415 too large；目标服务拒绝了本次请求大小（HTTP 413），请减少图片、执行 lattice session rewind 退回较早节点或新建会话";

        for (label, raw_msg, expected_reason) in [
            ("模型不支持图片输入", unsupported_raw, "不支持图片输入"),
            (
                "累计超过 32 MiB 本地请求体预算",
                budget_raw,
                "本地请求体预算（32 MiB）",
            ),
            (
                "远端 HTTP 413 拒绝",
                http_413_raw,
                "目标服务拒绝了本次请求大小（HTTP 413）",
            ),
        ] {
            let err = LocalError::new(LocalErrorKind::BadRequest, raw_msg);
            let rendered = describe_local_error(&err, &redactor);
            assert!(
                !rendered.contains("bot-secret-token-415"),
                "{label} 不得泄露机器人凭据：{rendered}"
            );
            assert!(
                rendered.contains(expected_reason),
                "{label} 应保留脱敏中文原因：{rendered}"
            );
            assert!(
                rendered.contains("减少图片")
                    && rendered.contains("/new")
                    && rendered.contains("lattice session rewind"),
                "{label} 必须同时引导减少图片、发送 /new 与执行 lattice session rewind：{rendered}"
            );
            assert_eq!(
                rendered.matches("减少图片").count(),
                1,
                "{label} 不得重复拼接两份引导说明：{rendered}"
            );
            assert_eq!(
                rendered.matches("lattice session rewind").count(),
                1,
                "{label} 不得重复出现两处 lattice session rewind：{rendered}"
            );
        }

        // 严格前缀/后缀反例核验：
        // 1. HTTP 404 正文携带 "model HTTP 413 not found" 或 "32 MiB" 字样绝不误判为图片/大小拒绝
        for lookalike in [
            "HTTP 404 not_found_error: model HTTP 413 not found",
            "HTTP 400 invalid_request_error: context window 32 MiB请求 exceeds limit",
            "HTTP 413 upstream raw text without lattice marker",
        ] {
            assert_eq!(
                format_backend_image_rejection(lookalike, &redactor),
                None,
                "非后台原生三类格式不得误触发图片拒绝引导：{lookalike}"
            );
            let bad_req = describe_local_error(
                &LocalError::new(LocalErrorKind::BadRequest, lookalike),
                &redactor,
            );
            assert!(
                !bad_req.contains("目标服务拒绝了本次请求大小") && !bad_req.contains("/new"),
                "伪 413 / 32 MiB 错误不得混入大小拒绝说明或 /new 引导：{bad_req}"
            );
        }

        // 2. 名为 local/HTTP 413 的纯文本模型发图被拒：仅输出不支持图片输入引导，绝不拼入 HTTP 413 大小拒绝说明
        let named_413_raw = "模型 local/HTTP 413 不支持图片输入；请改用支持图片的模型，或执行 lattice session rewind 退回不含图片的节点";
        let named_413_rendered = describe_local_error(
            &LocalError::new(LocalErrorKind::BadRequest, named_413_raw),
            &redactor,
        );
        assert_eq!(
            named_413_rendered,
            format!("模型 local/HTTP 413 不支持图片输入；{IM_IMAGE_REJECTION_GUIDANCE}"),
            "模型名包含 HTTP 413 时只能归入不支持图片输入分支"
        );
        assert!(!named_413_rendered.contains("目标服务拒绝了本次请求大小"));

        // 3. 超长模型名（600 个 'a'）在截断前识别，仅截断模型名并完整保留原因与引导，总字符数严格受控
        let long_model = format!("local/{}", "a".repeat(600));
        let long_unsupported_raw =
            format!("模型 {long_model} 不支持图片输入{CLI_UNSUPPORTED_IMAGE_SUFFIX}");
        let long_rendered = describe_local_error(
            &LocalError::new(LocalErrorKind::BadRequest, &long_unsupported_raw),
            &redactor,
        );
        assert_eq!(
            long_rendered.chars().count(),
            MAX_DIAGNOSTIC_CHARS,
            "超长模型名拒绝说明应精确控制在 MAX_DIAGNOSTIC_CHARS 内：{long_rendered}"
        );
        assert!(long_rendered.contains('…'));
        assert!(
            long_rendered.contains("不支持图片输入")
                && long_rendered.contains("减少图片")
                && long_rendered.contains("/new")
                && long_rendered.contains("lattice session rewind"),
            "超长模型名截断不得丢失不支持图片输入原因或任何操作引导：{long_rendered}"
        );

        // 4. 超长 HTTP 413 错误头（如大量短凭据替换为 [REDACTED] 后膨胀越过 512 字符）同样只截断 head
        let long_413_head = format!("HTTP 413 request_too_large: {}", "x".repeat(600));
        let long_413_raw =
            format!("{long_413_head}{HTTP_413_REASON_MARKER}{CLI_REQUEST_SIZE_SUFFIX}");
        let long_413_rendered =
            format_backend_image_rejection(&long_413_raw, &redactor).expect("长头 413 应成功识别");
        assert_eq!(long_413_rendered.chars().count(), MAX_DIAGNOSTIC_CHARS);
        assert!(
            long_413_rendered.starts_with("HTTP 413 request_too_large: ")
                && long_413_rendered.contains("…；目标服务拒绝了本次请求大小（HTTP 413）；")
                && long_413_rendered.ends_with(IM_IMAGE_REJECTION_GUIDANCE),
            "超长 HTTP 413 仅截断可变错误头并完整保留固定后缀与引导：{long_413_rendered}"
        );

        // 普通 Busy 与普通非图片/非大小类 BadRequest 保持原样不变
        assert_eq!(
            describe_local_error(
                &LocalError::new(LocalErrorKind::Busy, "session is busy"),
                &redactor,
            ),
            "正在忙，可发 /cancel"
        );
        assert_eq!(
            describe_local_error(
                &LocalError::new(
                    LocalErrorKind::BadRequest,
                    "request or frozen configuration is invalid",
                ),
                &redactor,
            ),
            "请求或会话配置无效"
        );
        assert_eq!(
            describe_local_error(
                &LocalError::new(
                    LocalErrorKind::BadRequest,
                    "model reference must be <provider>/<model>",
                ),
                &redactor,
            ),
            "请求或配置无效（model reference must be <provider>/<model>）"
        );
    }
}
