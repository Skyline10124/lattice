// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session 命令通过 LocalClient 操作后台；本层仅处理配置、参数与呈现。
//! 找回与查询先于当前 profile/provider 校验；引用写操作不在前端执行。
use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Lattice config root (`$XDG_CONFIG_HOME` or the Linux config directory).
pub(crate) fn lattice_config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg = xdg.trim();
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// CLI/TUI 的输入历史目录；领域数据由后台持有的全局 lattice.redb 存储。
pub(crate) fn default_session_dir() -> PathBuf {
    lattice_config_dir().join("lattice").join("sessions")
}

/// The private-state layout every durable store is addressed through
/// (ADR 0005 §3).
pub(crate) fn state_layout() -> Result<lattice_local::StateLayout> {
    Ok(lattice_local::StateLayout::resolve()?)
}

pub(crate) use crate::help::{
    parse_agent_name, parse_channel, parse_effort, parse_invocation_id, parse_model_ref,
    parse_node_id, parse_turn_id,
};

pub(crate) async fn local_client() -> Result<lattice_local::LocalClient> {
    let client = lattice_local::LocalClient::new(state_layout()?);
    client.status().await?;
    Ok(client)
}

#[derive(clap::Args, Debug)]
pub struct SendArgs {
    /// 文本消息，仅传 --image 时可省略
    #[arg(value_name = "消息", required_unless_present = "images")]
    pub prompt: Option<String>,
    /// 附带 PNG/JPEG/WebP/GIF 图片，可重复
    #[arg(long = "image", value_name = "路径")]
    pub images: Vec<PathBuf>,
    /// 续写已有会话，省略时新建会话
    #[arg(long = "session", value_name = "会话ID")]
    pub session_id: Option<String>,
    /// 指定 Agent，新建会话时必填
    #[arg(
        long,
        value_name = "Agent",
        required_unless_present = "session_id",
        value_parser = parse_agent_name
    )]
    pub agent: Option<lattice_local::AgentName>,
    /// 模型引用 <provider>/<model>，省略时继承
    #[arg(long, value_name = "模型", value_parser = parse_model_ref)]
    pub model: Option<lattice_local::ModelRef>,
    #[arg(
        long,
        value_name = "档位",
        value_parser = parse_effort,
        help = effort_help()
    )]
    pub effort: Option<lattice_local::Effort>,
    /// 来源渠道标签，默认 cli
    #[arg(
        long = "channel",
        value_name = "渠道",
        default_value = "cli",
        hide_default_value = true,
        value_parser = parse_channel
    )]
    pub channel: lattice_local::Channel,
    /// 要求会话当前位置匹配此节点号，须配合 --session
    #[arg(
        long = "expected-position",
        value_name = "节点号",
        requires = "session_id",
        value_parser = parse_node_id
    )]
    pub expected_position: Option<lattice_local::NodeId>,
}

#[derive(clap::Args, Debug)]
pub struct FeedArgs {
    /// 会话 ID 或唯一前缀
    #[arg(value_name = "会话ID")]
    pub session_id: String,
    /// 文本消息，仅传 --image 时可省略
    #[arg(value_name = "消息", required_unless_present = "images")]
    pub prompt: Option<String>,
    /// 附带 PNG/JPEG/WebP/GIF 图片，可重复
    #[arg(long = "image", value_name = "路径")]
    pub images: Vec<PathBuf>,
    /// 来源渠道标签，默认 cli
    #[arg(
        long = "channel",
        value_name = "渠道",
        default_value = "cli",
        hide_default_value = true,
        value_parser = parse_channel
    )]
    pub channel: lattice_local::Channel,
    /// 要求会话当前位置匹配此节点号
    #[arg(
        long = "expected-position",
        value_name = "节点号",
        value_parser = parse_node_id
    )]
    pub expected_position: Option<lattice_local::NodeId>,
}

/// 按命令行出现顺序记录 `session context-op` 的原始编辑标志与 `--image` 标志。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawContextOpToken {
    OmitNode(lattice_local::NodeId),
    ReplaceInput {
        node: String,
        text: String,
    },
    Image(PathBuf),
    ReplaceStep {
        node: String,
        text: String,
    },
    OmitInvocation(lattice_local::InvocationId),
    ReplaceToolResult {
        invocation: String,
        text: String,
    },
    StripReasoning(lattice_local::NodeId),
    OmitImage {
        node: String,
        image: String,
    },
    ReplaceImage {
        node: String,
        image: String,
        text: String,
    },
}

#[derive(clap::Args, Debug)]
pub struct ContextOpArgs {
    /// 会话 ID 或唯一前缀
    #[arg(value_name = "会话ID")]
    pub session_id: String,
    /// 移出整个节点，可重复
    #[arg(
        long = "omit-node",
        value_name = "节点号",
        action = clap::ArgAction::Append,
        value_parser = parse_node_id
    )]
    pub omit_node: Vec<lattice_local::NodeId>,
    /// 替换用户输入节点，后可紧跟 --image，可重复
    #[arg(
        long = "replace-input",
        num_args = 2,
        value_names = ["节点号", "文本"],
        action = clap::ArgAction::Append
    )]
    pub replace_input: Vec<String>,
    /// 为前一个 --replace-input 附带图片，可重复
    #[arg(
        long = "image",
        value_name = "路径",
        action = clap::ArgAction::Append
    )]
    pub images: Vec<PathBuf>,
    /// 替换步骤节点的助手文本，可重复
    #[arg(
        long = "replace-step",
        num_args = 2,
        value_names = ["节点号", "文本"],
        action = clap::ArgAction::Append
    )]
    pub replace_step: Vec<String>,
    /// 成对移出工具调用及其结果，可重复
    #[arg(
        long = "omit-invocation",
        value_name = "调用ID",
        action = clap::ArgAction::Append,
        value_parser = parse_invocation_id
    )]
    pub omit_invocation: Vec<lattice_local::InvocationId>,
    /// 替换工具调用的结果文本，可重复
    #[arg(
        long = "replace-tool-result",
        num_args = 2,
        value_names = ["调用ID", "文本"],
        action = clap::ArgAction::Append
    )]
    pub replace_tool_result: Vec<String>,
    /// 剥离步骤节点的推理内容，可重复
    #[arg(
        long = "strip-reasoning",
        value_name = "节点号",
        action = clap::ArgAction::Append,
        value_parser = parse_node_id
    )]
    pub strip_reasoning: Vec<lattice_local::NodeId>,
    /// 移出输入节点图片，格式 <类型>:<字节>:<sha256> 或 @<路径>
    #[arg(
        long = "omit-image",
        num_args = 2,
        value_names = ["节点号", "图片"],
        action = clap::ArgAction::Append
    )]
    pub omit_image: Vec<String>,
    /// 将输入节点图片替换为文本，格式同 --omit-image
    #[arg(
        long = "replace-image",
        num_args = 3,
        value_names = ["节点号", "图片", "文本"],
        action = clap::ArgAction::Append
    )]
    pub replace_image: Vec<String>,
    /// 来源渠道标签，默认 cli
    #[arg(
        long = "channel",
        value_name = "渠道",
        default_value = "cli",
        hide_default_value = true,
        value_parser = parse_channel
    )]
    pub channel: lattice_local::Channel,
    /// 要求会话当前位置匹配此节点号
    #[arg(
        long = "expected-position",
        value_name = "节点号",
        value_parser = parse_node_id
    )]
    pub expected_position: Option<lattice_local::NodeId>,
    /// 按命令行参数位置抽取的有序原始编辑项。
    #[arg(skip)]
    pub ordered_tokens: Vec<RawContextOpToken>,
}

#[derive(clap::Args, Debug)]
pub struct TriggerArgs {
    /// 会话 ID 或唯一前缀
    #[arg(value_name = "会话ID")]
    pub session_id: String,
    /// 触发前切换 Agent
    #[arg(long, value_name = "Agent", value_parser = parse_agent_name)]
    pub agent: Option<lattice_local::AgentName>,
    /// 模型引用 <provider>/<model>，省略时继承
    #[arg(long, value_name = "模型", value_parser = parse_model_ref)]
    pub model: Option<lattice_local::ModelRef>,
    #[arg(
        long,
        value_name = "档位",
        value_parser = parse_effort,
        help = effort_help()
    )]
    pub effort: Option<lattice_local::Effort>,
    /// 来源渠道标签，默认 cli
    #[arg(
        long = "channel",
        value_name = "渠道",
        default_value = "cli",
        hide_default_value = true,
        value_parser = parse_channel
    )]
    pub channel: lattice_local::Channel,
    /// 要求会话当前位置匹配此节点号
    #[arg(
        long = "expected-position",
        value_name = "节点号",
        value_parser = parse_node_id
    )]
    pub expected_position: Option<lattice_local::NodeId>,
}

pub(crate) fn populate_context_op_tokens(
    args: &mut ContextOpArgs,
    root_matches: &clap::ArgMatches,
) {
    let Some(sub) = root_matches
        .subcommand_matches("session")
        .and_then(|session| session.subcommand_matches("context-op"))
    else {
        return;
    };
    let mut positioned: Vec<(usize, RawContextOpToken)> = Vec::new();

    if let (Some(indices), Some(values)) = (
        sub.indices_of("omit_node"),
        sub.get_many::<lattice_local::NodeId>("omit_node"),
    ) {
        for (idx, node) in indices.zip(values) {
            positioned.push((idx, RawContextOpToken::OmitNode(*node)));
        }
    }
    if let (Some(indices), Some(values)) = (
        sub.indices_of("replace_input"),
        sub.get_many::<String>("replace_input"),
    ) {
        let idxs: Vec<usize> = indices.collect();
        let vals: Vec<&String> = values.collect();
        for (idx_chunk, val_chunk) in idxs.chunks_exact(2).zip(vals.chunks_exact(2)) {
            positioned.push((
                idx_chunk[0],
                RawContextOpToken::ReplaceInput {
                    node: val_chunk[0].clone(),
                    text: val_chunk[1].clone(),
                },
            ));
        }
    }
    if let (Some(indices), Some(values)) =
        (sub.indices_of("images"), sub.get_many::<PathBuf>("images"))
    {
        for (idx, path) in indices.zip(values) {
            positioned.push((idx, RawContextOpToken::Image(path.clone())));
        }
    }
    if let (Some(indices), Some(values)) = (
        sub.indices_of("replace_step"),
        sub.get_many::<String>("replace_step"),
    ) {
        let idxs: Vec<usize> = indices.collect();
        let vals: Vec<&String> = values.collect();
        for (idx_chunk, val_chunk) in idxs.chunks_exact(2).zip(vals.chunks_exact(2)) {
            positioned.push((
                idx_chunk[0],
                RawContextOpToken::ReplaceStep {
                    node: val_chunk[0].clone(),
                    text: val_chunk[1].clone(),
                },
            ));
        }
    }
    if let (Some(indices), Some(values)) = (
        sub.indices_of("omit_invocation"),
        sub.get_many::<lattice_local::InvocationId>("omit_invocation"),
    ) {
        for (idx, invocation) in indices.zip(values) {
            positioned.push((idx, RawContextOpToken::OmitInvocation(*invocation)));
        }
    }
    if let (Some(indices), Some(values)) = (
        sub.indices_of("replace_tool_result"),
        sub.get_many::<String>("replace_tool_result"),
    ) {
        let idxs: Vec<usize> = indices.collect();
        let vals: Vec<&String> = values.collect();
        for (idx_chunk, val_chunk) in idxs.chunks_exact(2).zip(vals.chunks_exact(2)) {
            positioned.push((
                idx_chunk[0],
                RawContextOpToken::ReplaceToolResult {
                    invocation: val_chunk[0].clone(),
                    text: val_chunk[1].clone(),
                },
            ));
        }
    }
    if let (Some(indices), Some(values)) = (
        sub.indices_of("strip_reasoning"),
        sub.get_many::<lattice_local::NodeId>("strip_reasoning"),
    ) {
        for (idx, node) in indices.zip(values) {
            positioned.push((idx, RawContextOpToken::StripReasoning(*node)));
        }
    }
    if let (Some(indices), Some(values)) = (
        sub.indices_of("omit_image"),
        sub.get_many::<String>("omit_image"),
    ) {
        let idxs: Vec<usize> = indices.collect();
        let vals: Vec<&String> = values.collect();
        for (idx_chunk, val_chunk) in idxs.chunks_exact(2).zip(vals.chunks_exact(2)) {
            positioned.push((
                idx_chunk[0],
                RawContextOpToken::OmitImage {
                    node: val_chunk[0].clone(),
                    image: val_chunk[1].clone(),
                },
            ));
        }
    }
    if let (Some(indices), Some(values)) = (
        sub.indices_of("replace_image"),
        sub.get_many::<String>("replace_image"),
    ) {
        let idxs: Vec<usize> = indices.collect();
        let vals: Vec<&String> = values.collect();
        for (idx_chunk, val_chunk) in idxs.chunks_exact(3).zip(vals.chunks_exact(3)) {
            positioned.push((
                idx_chunk[0],
                RawContextOpToken::ReplaceImage {
                    node: val_chunk[0].clone(),
                    image: val_chunk[1].clone(),
                    text: val_chunk[2].clone(),
                },
            ));
        }
    }

    positioned.sort_by_key(|(idx, _)| *idx);
    args.ordered_tokens = positioned.into_iter().map(|(_, token)| token).collect();
}

fn effort_help() -> String {
    let efforts = lattice_local::Effort::ALL
        .into_iter()
        .map(lattice_local::Effort::as_str)
        .collect::<Vec<_>>()
        .join("/");
    format!("推理档位：{efforts}，省略时继承")
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// 向会话提交一轮对话
    Send(SendArgs),
    /// 向会话追加用户输入，不触发推理
    Feed(FeedArgs),
    /// 向会话追加上下文编辑操作
    ContextOp(ContextOpArgs),
    /// 基于当前有效上下文触发新一轮推理
    Trigger(TriggerArgs),
    /// 查看有效模型上下文统计
    Inspect {
        /// 会话 ID 或唯一前缀
        #[arg(value_name = "会话ID")]
        session_id: String,
        /// 检查指定节点位置，默认当前位置
        #[arg(long = "position", value_name = "节点号", value_parser = parse_node_id)]
        position: Option<lattice_local::NodeId>,
    },
    /// 将会话当前位置退回到指定节点，保留原分支且不回退文件
    Rewind {
        /// 会话 ID 或唯一前缀
        #[arg(value_name = "会话ID")]
        session_id: String,
        /// 目标节点序号，根节点为 0
        #[arg(value_name = "节点号", value_parser = parse_node_id)]
        target: lattice_local::NodeId,
    },
    /// 列出全部会话
    List,
    /// 查看完整会话树
    Log {
        /// 会话 ID 或唯一前缀
        #[arg(value_name = "会话ID")]
        session_id: String,
    },
    /// 查看当前路径逐轮用量与等量 API 费用，未知项显示 -
    Usage {
        /// 会话 ID 或唯一前缀
        #[arg(value_name = "会话ID")]
        session_id: String,
    },
    /// 按轮次 ID 查看单轮步骤与状态
    Turn {
        /// 轮次 TurnId UUID
        #[arg(value_name = "轮次ID", value_parser = parse_turn_id)]
        turn_id: lattice_local::TurnId,
        /// 持续观察该轮直至结束
        #[arg(long)]
        follow: bool,
    },
    /// 取消会话当前正在执行的一轮
    Cancel {
        /// 会话 ID 或唯一前缀
        #[arg(value_name = "会话ID")]
        session_id: String,
    },
}

fn current_workspace() -> Result<PathBuf, lattice_local::LocalError> {
    std::env::current_dir().map_err(|_| {
        lattice_local::LocalError::new(
            lattice_local::LocalErrorKind::Internal,
            "operation failed; inspect durable state",
        )
    })
}

fn bad_request(message: impl Into<String>) -> lattice_local::LocalError {
    lattice_local::LocalError::new(lattice_local::LocalErrorKind::BadRequest, &message.into())
}

fn parse_image_spec(spec: &str) -> Result<lattice_local::ImageRef, lattice_local::LocalError> {
    if let Some(raw_path) = spec.strip_prefix('@') {
        if raw_path.is_empty() {
            return Err(bad_request("图片路径不能为空"));
        }
        return lattice_local::image_file_ref(1, std::path::Path::new(raw_path));
    }
    let mut parts = spec.split(':');
    let (Some(media_raw), Some(len_raw), Some(hash_raw), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad_request(
            "图片引用格式无效，必须为 <media_type>:<byte_length>:<sha256_hex> 或 @<PATH>",
        ));
    };
    let media_type = match media_raw {
        "png" => lattice_local::ImageMediaType::Png,
        "jpeg" | "jpg" => lattice_local::ImageMediaType::Jpeg,
        "webp" => lattice_local::ImageMediaType::Webp,
        "gif" => lattice_local::ImageMediaType::Gif,
        _ => {
            return Err(bad_request(format!(
                "不支持的图片类型 `{media_raw}`，仅支持 png、jpeg/jpg、webp、gif"
            )));
        }
    };
    if len_raw.is_empty() || !len_raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad_request("图片引用中的字节长度必须为正整数"));
    }
    let byte_length = len_raw
        .parse::<u64>()
        .map_err(|_| bad_request("图片引用中的字节长度超出范围"))?;
    let max_bytes =
        u64::try_from(lattice_local::MAX_IMAGE_BYTES).expect("MAX_IMAGE_BYTES 可转 u64");
    if byte_length == 0 || byte_length > max_bytes {
        return Err(bad_request(format!(
            "图片为 {byte_length} 字节，必须大于 0 且不能超过 3,932,160 字节（3.75 MiB）"
        )));
    }
    let sha256 = parse_sha256_hex(hash_raw)?;
    Ok(lattice_local::ImageRef {
        sha256,
        media_type,
        byte_length,
    })
}

fn parse_sha256_hex(hex: &str) -> Result<[u8; 32], lattice_local::LocalError> {
    if hex.len() != 64 || !hex.is_ascii() {
        return Err(bad_request("图片 SHA-256 必须为 64 位十六进制字符串"));
    }
    let bytes = hex.as_bytes();
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        let hi = decode_hex_nibble(pair[0])
            .ok_or_else(|| bad_request("图片 SHA-256 包含非十六进制字符"))?;
        let lo = decode_hex_nibble(pair[1])
            .ok_or_else(|| bad_request("图片 SHA-256 包含非十六进制字符"))?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn image_media_type_label(media_type: lattice_local::ImageMediaType) -> &'static str {
    match media_type {
        lattice_local::ImageMediaType::Png => "png",
        lattice_local::ImageMediaType::Jpeg => "jpeg",
        lattice_local::ImageMediaType::Webp => "webp",
        lattice_local::ImageMediaType::Gif => "gif",
    }
}

fn format_image_ref(image: &lattice_local::ImageRef) -> String {
    let mut out = String::with_capacity(80);
    out.push_str(image_media_type_label(image.media_type));
    out.push(':');
    out.push_str(&image.byte_length.to_string());
    out.push(':');
    for byte in image.sha256 {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn build_context_op_plan_parts(
    channel: lattice_local::Channel,
    tokens: &[RawContextOpToken],
) -> Result<
    (
        lattice_local::ContextOp,
        Vec<(lattice_local::ImageRef, PathBuf)>,
    ),
    lattice_local::LocalError,
> {
    if tokens.is_empty() {
        return Err(bad_request("至少需要指定一条上下文编辑操作"));
    }

    let mut edits = Vec::new();
    let mut unique_images: Vec<(lattice_local::ImageRef, PathBuf)> = Vec::new();
    let mut seen_digests = std::collections::BTreeSet::<[u8; 32]>::new();
    let mut image_ordinal: usize = 0;

    let mut index = 0;
    while index < tokens.len() {
        match &tokens[index] {
            RawContextOpToken::Image(_) => {
                return Err(bad_request("--image 只能紧跟在 --replace-input 之后"));
            }
            RawContextOpToken::OmitNode(node) => {
                edits.push(lattice_local::ContextEdit::OmitNode { node: *node });
                index += 1;
            }
            RawContextOpToken::ReplaceInput { node, text } => {
                let node = parse_node_id(node).map_err(bad_request)?;
                let mut parts = Vec::new();
                if !text.is_empty() {
                    parts.push(lattice_local::MessageSemantic::Text { text: text.clone() });
                }
                index += 1;
                while let Some(RawContextOpToken::Image(path)) = tokens.get(index) {
                    image_ordinal += 1;
                    let image = lattice_local::image_file_ref(image_ordinal, path)?;
                    parts.push(lattice_local::MessageSemantic::Image { image });
                    if seen_digests.insert(image.sha256) {
                        unique_images.push((image, path.clone()));
                    }
                    index += 1;
                }
                let edit = lattice_local::ContextEdit::replace_input(node, parts)
                    .map_err(|e| bad_request(e.to_string()))?;
                edits.push(edit);
            }
            RawContextOpToken::ReplaceStep { node, text } => {
                let node = parse_node_id(node).map_err(bad_request)?;
                let text = lattice_local::ContextText::new(text.clone())
                    .map_err(|e| bad_request(e.to_string()))?;
                edits.push(lattice_local::ContextEdit::ReplaceStep { node, text });
                index += 1;
            }
            RawContextOpToken::OmitInvocation(invocation) => {
                edits.push(lattice_local::ContextEdit::OmitInvocation {
                    invocation: *invocation,
                });
                index += 1;
            }
            RawContextOpToken::ReplaceToolResult { invocation, text } => {
                let invocation = parse_invocation_id(invocation).map_err(bad_request)?;
                let text = lattice_local::ToolResultText::new(text.clone())
                    .map_err(|e| bad_request(e.to_string()))?;
                edits.push(lattice_local::ContextEdit::ReplaceToolResult { invocation, text });
                index += 1;
            }
            RawContextOpToken::StripReasoning(node) => {
                edits.push(lattice_local::ContextEdit::StripReasoning { node: *node });
                index += 1;
            }
            RawContextOpToken::OmitImage { node, image } => {
                let node = parse_node_id(node).map_err(bad_request)?;
                let image = parse_image_spec(image)?;
                edits.push(lattice_local::ContextEdit::ReplaceImage {
                    node,
                    image,
                    text: None,
                });
                index += 1;
            }
            RawContextOpToken::ReplaceImage { node, image, text } => {
                let node = parse_node_id(node).map_err(bad_request)?;
                let image = parse_image_spec(image)?;
                let text = lattice_local::ContextText::new(text.clone())
                    .map_err(|e| bad_request(e.to_string()))?;
                edits.push(lattice_local::ContextEdit::ReplaceImage {
                    node,
                    image,
                    text: Some(text),
                });
                index += 1;
            }
        }
    }

    let op =
        lattice_local::ContextOp::new(channel, edits).map_err(|e| bad_request(e.to_string()))?;
    Ok((op, unique_images))
}

pub(super) async fn execute_feed(
    api: Option<&lattice_local::LocalClient>,
    args: FeedArgs,
    workspace_override: Option<PathBuf>,
    json: bool,
    out: &mut impl std::io::Write,
) -> Result<std::process::ExitCode> {
    let workspace = match workspace_override {
        Some(path) => path,
        None => current_workspace()?,
    };
    let mut planned_refs = Vec::with_capacity(args.images.len());
    let mut plan_parts = Vec::new();
    if let Some(text) = &args.prompt {
        plan_parts.push(lattice_local::UserInputPart::Text { text: text.clone() });
    }
    for (index, path) in args.images.iter().enumerate() {
        let image = lattice_local::image_file_ref(index + 1, path)?;
        planned_refs.push(image);
        plan_parts.push(lattice_local::UserInputPart::Image { image });
    }
    let placeholder = lattice_local::new_session_id();
    let local_plan = lattice_local::PlannedFeedRequest {
        session: placeholder,
        workspace: workspace.clone(),
        channel: args.channel.clone(),
        parts: plan_parts.clone(),
        expected_position: args.expected_position,
    };
    lattice_local::check_feed_plan(&local_plan)?;

    let owned_client;
    let client = match api {
        Some(client) => client,
        None => {
            owned_client = local_client().await?;
            &owned_client
        }
    };
    let session_id = client.resolve_session(&args.session_id).await?.session_id;
    let verified_plan = lattice_local::PlannedFeedRequest {
        session: session_id,
        workspace: workspace.clone(),
        channel: args.channel.clone(),
        parts: plan_parts,
        expected_position: args.expected_position,
    };
    lattice_local::check_feed_plan(&verified_plan)?;

    let mut parts = Vec::new();
    if let Some(text) = args.prompt {
        parts.push(lattice_local::UserInputPart::Text { text });
    }
    for (index, (path, expected)) in args.images.iter().zip(&planned_refs).enumerate() {
        let bytes = lattice_local::read_verified_image(index + 1, path, expected)?;
        parts.push(lattice_local::UserInputPart::Image { image: bytes });
    }
    let reply = client
        .feed(lattice_local::FeedRequest {
            session: session_id,
            workspace,
            channel: args.channel,
            parts,
            expected_position: args.expected_position,
        })
        .await?;
    if json {
        writeln!(out, "{}", serde_json::to_string(&reply)?)?;
    } else {
        writeln!(out, "session={} node={}", reply.session_id, reply.node.0)?;
    }
    Ok(std::process::ExitCode::SUCCESS)
}

pub(super) async fn execute_context_op(
    api: Option<&lattice_local::LocalClient>,
    args: ContextOpArgs,
    workspace_override: Option<PathBuf>,
    json: bool,
    out: &mut impl std::io::Write,
) -> Result<std::process::ExitCode> {
    let workspace = match workspace_override {
        Some(path) => path,
        None => current_workspace()?,
    };
    let (op, unique_images) = build_context_op_plan_parts(args.channel, &args.ordered_tokens)?;
    let planned_images: Vec<lattice_local::ContextImage<lattice_local::ImageRef>> = unique_images
        .iter()
        .map(|(image, _)| lattice_local::ContextImage { image: *image })
        .collect();
    let placeholder = lattice_local::new_session_id();
    let local_plan = lattice_local::PlannedContextOpRequest {
        session: placeholder,
        workspace: workspace.clone(),
        op: op.clone(),
        images: planned_images.clone(),
        expected_position: args.expected_position,
    };
    lattice_local::check_context_op_plan(&local_plan)?;

    let owned_client;
    let client = match api {
        Some(client) => client,
        None => {
            owned_client = local_client().await?;
            &owned_client
        }
    };
    let session_id = client.resolve_session(&args.session_id).await?.session_id;
    let verified_plan = lattice_local::PlannedContextOpRequest {
        session: session_id,
        workspace: workspace.clone(),
        op: op.clone(),
        images: planned_images,
        expected_position: args.expected_position,
    };
    lattice_local::check_context_op_plan(&verified_plan)?;

    let mut images = Vec::with_capacity(unique_images.len());
    for (index, (expected, path)) in unique_images.iter().enumerate() {
        let bytes = lattice_local::read_verified_image(index + 1, path, expected)?;
        images.push(lattice_local::ContextImage { image: bytes });
    }
    let reply = client
        .context_op(lattice_local::ContextOpRequest {
            session: session_id,
            workspace,
            op,
            images,
            expected_position: args.expected_position,
        })
        .await?;
    if json {
        writeln!(out, "{}", serde_json::to_string(&reply)?)?;
    } else {
        writeln!(out, "session={} node={}", reply.session_id, reply.node.0)?;
    }
    Ok(std::process::ExitCode::SUCCESS)
}

pub(super) async fn execute_inspect(
    api: &lattice_local::LocalClient,
    session_id: &str,
    position: Option<lattice_local::NodeId>,
    json: bool,
    out: &mut impl std::io::Write,
) -> Result<std::process::ExitCode> {
    let id = api.resolve_session(session_id).await?.session_id;
    let reply = api.inspect(id, position).await?;
    if json {
        writeln!(out, "{}", serde_json::to_string(&reply)?)?;
    } else {
        write!(out, "{}", render_inspect(&reply))?;
    }
    Ok(std::process::ExitCode::SUCCESS)
}

pub async fn run(
    action: SessionAction,
    json: bool,
    verbose: bool,
) -> Result<std::process::ExitCode> {
    match action {
        SessionAction::Feed(args) => {
            return execute_feed(None, args, None, json, &mut std::io::stdout()).await;
        }
        SessionAction::ContextOp(args) => {
            return execute_context_op(None, args, None, json, &mut std::io::stdout()).await;
        }
        action => {
            let api = local_client().await?;
            match action {
                SessionAction::Send(args) => {
                    return crate::commands::run::run_session_prompt(&api, args, verbose, json)
                        .await;
                }
                SessionAction::Trigger(args) => {
                    return crate::commands::run::run_session_trigger(&api, args, verbose, json)
                        .await;
                }
                SessionAction::Inspect {
                    session_id,
                    position,
                } => {
                    return execute_inspect(
                        &api,
                        &session_id,
                        position,
                        json,
                        &mut std::io::stdout(),
                    )
                    .await;
                }
                SessionAction::Turn { turn_id, follow } => {
                    return show_turn(
                        &api,
                        turn_id,
                        follow,
                        json,
                        verbose,
                        &mut std::io::stdout(),
                        &mut std::io::stderr(),
                    )
                    .await;
                }
                SessionAction::Cancel { session_id } => {
                    return cancel_current_turn(
                        &api,
                        &session_id,
                        json,
                        &mut std::io::stdout(),
                        &mut std::io::stderr(),
                    )
                    .await;
                }
                SessionAction::List => {
                    let rows = api.list_sessions().await?;
                    if json {
                        println!("{}", serde_json::to_string(&rows)?);
                    } else {
                        for row in rows {
                            println!(
                                "{} project={} current={} nodes={} unclosed_turn={} owner={} updated={}",
                                display_id(&row.session.session_id.to_string()),
                                row.session.project_path.to_string_lossy(),
                                row.session.current.0,
                                row.session.node_count,
                                row.session
                                    .unclosed_turn
                                    .map_or_else(|| "none".into(), |id| id.to_string()),
                                row.owner,
                                row.session.updated_at
                            );
                        }
                    }
                }
                SessionAction::Log { session_id } => {
                    let id = api.resolve_session(&session_id).await?.session_id;
                    let log = api.log(id).await?;
                    if json {
                        println!("{}", serde_json::to_string(&log)?);
                    } else {
                        print!("{}", render_log(&log));
                    }
                }
                SessionAction::Usage { session_id } => {
                    let usage = api.usage(&session_id).await?;
                    if json {
                        println!("{}", serde_json::to_string(&usage)?);
                    } else {
                        print!("{}", render_usage(&usage));
                    }
                }
                SessionAction::Rewind { session_id, target } => {
                    let id = api.resolve_session(&session_id).await?.session_id;
                    let workspace = current_workspace()?;
                    let session = api.rewind(id, target, workspace).await?;
                    if json {
                        println!("{}", serde_json::to_string(&session)?);
                    } else {
                        println!(
                            "session {} current={}; files were not rolled back",
                            session.session_id, session.current.0
                        );
                    }
                }
                SessionAction::Feed(_) | SessionAction::ContextOp(_) => {
                    unreachable!("Feed 与 ContextOp 已在连接后台前分派")
                }
            }
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

pub(super) async fn show_turn(
    api: &lattice_local::LocalClient,
    turn_id: lattice_local::TurnId,
    follow: bool,
    json: bool,
    verbose: bool,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> Result<std::process::ExitCode> {
    let detail = if follow {
        api.wait_for_turn(turn_id).await?
    } else {
        match api.turn(turn_id).await? {
            lattice_local::TurnLookup::Accepted { turn } => turn,
            lattice_local::TurnLookup::InFlight => {
                if json {
                    writeln!(
                        out,
                        "{}",
                        serde_json::json!({"status":"in_flight","turn_id":turn_id})
                    )?;
                } else {
                    writeln!(out, "turn {turn_id}: in flight; admission is still pending")?;
                }
                return Ok(std::process::ExitCode::SUCCESS);
            }
        }
    };
    if follow {
        return super::run::follow_with_output(api, detail, verbose, json, out, err).await;
    }
    if json {
        writeln!(out, "{}", serde_json::to_string(&detail)?)?;
    } else {
        write!(out, "{}", render_turn(&detail))?;
    }
    Ok(std::process::ExitCode::SUCCESS)
}
pub(super) async fn cancel_current_turn(
    api: &lattice_local::LocalClient,
    prefix: &str,
    json: bool,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> Result<std::process::ExitCode> {
    use lattice_local::{CancelStatus, LocalUpdate};
    let session_id = api.resolve_session(prefix).await?.session_id;
    let head = api.session_head(session_id).await?;
    // 查头后就固定目标；回执丢失不能丢掉本次取消的身份，也不能追取消新一轮。
    let Some(turn_id) = head.turn_id.filter(|_| head.owner) else {
        if json {
            writeln!(
                out,
                "{}",
                serde_json::json!({"type":"cancel","session_id":session_id,"turn_id":null,"status":"none"})
            )?;
        } else {
            writeln!(out, "session {session_id}: no running turn to cancel")?;
        }
        return Ok(std::process::ExitCode::SUCCESS);
    };
    let (status, owned, error) = match api.cancel(turn_id).await {
        Ok(true) => (CancelStatus::Taken, Some(true), None),
        Ok(false) => (CancelStatus::NotTaken, Some(false), None),
        Err(error) => (CancelStatus::Unknown, None, Some(error)),
    };
    if json {
        let mut record = serde_json::to_value(LocalUpdate::Cancel {
            turn_id,
            status,
            owned,
            error: error.clone(),
        })?;
        record["session_id"] = serde_json::to_value(session_id)?;
        writeln!(out, "{record}")?;
    } else {
        let message = match status {
            CancelStatus::Taken => "taken; the durable outcome is still to be queried",
            CancelStatus::NotTaken => "not_taken; background work may still exist",
            _ => "unknown; query the original TurnId",
        };
        writeln!(out, "session {session_id} turn {turn_id}: {message}")?;
        if let Some(error) = &error {
            super::run::render_human_error(&serde_json::to_value(error)?, err)?;
        }
    }
    Ok(std::process::ExitCode::from(u8::from(error.is_some())))
}

fn render_inspect(reply: &lattice_local::InspectReply) -> String {
    use std::fmt::Write;
    let info = &reply.inspection;
    let mut text = format!("session {} @{}\n", reply.session_id, reply.position.0);
    let _ = writeln!(
        text,
        "有效条目：{}  已操作节点：{}  有效文本总字节：{}  有效图片：{} 张（{} 字节）",
        info.entries.len(),
        info.operated_nodes.len(),
        info.total_text_bytes,
        info.images.len(),
        info.total_image_bytes
    );

    if info.channel_counts.is_empty() {
        text.push_str("通道计数：无\n");
    } else {
        let channels = info
            .channel_counts
            .iter()
            .map(|(ch, count)| format!("{ch}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(text, "通道计数：{channels}");
    }

    let mut status_by_node = std::collections::BTreeMap::new();
    if info.operated_nodes.is_empty() {
        text.push_str("已操作节点：无\n");
    } else {
        let nodes = info
            .operated_nodes
            .iter()
            .map(|item| {
                status_by_node.insert(item.node, item.status);
                let label = match item.status {
                    lattice_local::NodeStatus::Omitted => "已移出",
                    lattice_local::NodeStatus::Modified => "已修改",
                    lattice_local::NodeStatus::SkippedEmpty => "已置空跳过",
                };
                format!("@{} ({label})", item.node.0)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(text, "已操作节点：{nodes}");
    }

    if info.images.is_empty() {
        text.push_str("有效图片列表：无\n");
    } else {
        text.push_str("有效图片列表：\n");
        for image in &info.images {
            let _ = writeln!(text, "  {}", format_image_ref(image));
        }
    }

    if info.entries.is_empty() {
        text.push_str("有效条目明细：无\n");
    } else {
        text.push_str("有效条目明细：\n");
        for entry in &info.entries {
            match entry {
                lattice_local::EffectiveEntrySummary::Input {
                    node,
                    channel,
                    text_bytes,
                    image_count,
                    ..
                } => {
                    let state = match status_by_node.get(node) {
                        Some(lattice_local::NodeStatus::Modified) => "已修改",
                        _ => "原样",
                    };
                    let _ = writeln!(
                        text,
                        "  @{} input channel={} 状态={} 文本={} 图片={}",
                        node.0, channel, state, text_bytes, image_count
                    );
                }
                lattice_local::EffectiveEntrySummary::Step {
                    node,
                    text_bytes,
                    reasoning_bytes,
                    tool_call_count,
                    tool_result_bytes,
                } => {
                    let state = match status_by_node.get(node) {
                        Some(lattice_local::NodeStatus::Modified) => "已修改",
                        _ => "原样",
                    };
                    let reasoning = if *reasoning_bytes > 0 { "有" } else { "无" };
                    let _ = writeln!(
                        text,
                        "  @{} step 状态={} 文本={} 推理={} 工具调用={} 工具结果={}",
                        node.0, state, text_bytes, reasoning, tool_call_count, tool_result_bytes
                    );
                }
            }
        }
    }
    text
}

fn render_log(log: &lattice_local::SessionLog) -> String {
    use std::fmt::Write;
    let mut text = format!(
        "session {} current={} agent={} unclosed_turn={}\n",
        log.session.session_id,
        log.session.current.0,
        summarize(
            log.current_agent.as_ref().map_or("none", |a| a.as_str()),
            120
        ),
        log.unclosed_turn
            .map_or_else(|| "none".into(), |id| id.to_string())
    );
    let mut children = std::collections::BTreeMap::<_, Vec<_>>::new();
    for row in &log.nodes {
        children.entry(row.node.parent).or_default().push(row);
    }
    let mut stack: Vec<_> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|row| (*row, 0))
        .collect();
    while let Some((row, depth)) = stack.pop() {
        let kind = match row.node.kind {
            lattice_local::NodeKind::Root => "root".into(),
            lattice_local::NodeKind::Input => "input".into(),
            lattice_local::NodeKind::ContextOp => "context_op".into(),
            lattice_local::NodeKind::Step { ordinal } => format!("step {ordinal}"),
        };
        let _ = writeln!(
            text,
            "{}{} {} {kind}",
            "  ".repeat(depth),
            if row.current { "*" } else { " " },
            row.node.id.0
        );
        if let Some(turn) = &row.turn_start {
            let _ = writeln!(
                text,
                "{}turn {} agent={} summary={}",
                "  ".repeat(depth + 1),
                turn.turn_id,
                turn.agent_name,
                serde_json::to_string(&turn.summary).expect("summary")
            );
        }
        if let Some(rows) = children.get(&Some(row.node.id)) {
            stack.extend(rows.iter().rev().map(|child| (*child, depth + 1)));
        }
    }
    text
}

fn render_usage(usage: &lattice_local::SessionUsage) -> String {
    use std::fmt::Write;
    let mut text = format!(
        "session {} position={}\n",
        usage.session_id, usage.position.0
    );
    text.push_str("NODE\tTURN\tPROVIDER/MODEL\tATTEMPTS\tINPUT\tCACHE_READ\tCACHE_WRITE\tOUTPUT\tREASONING\tCOST USD\n");
    for turn in &usage.turns {
        let _ = writeln!(
            text,
            "@{}\t{}\t{}/{}\t{}",
            turn.start_node.0,
            turn.turn_id,
            turn.provider,
            summarize(turn.model.as_str(), 120),
            render_usage_values(&turn.usage)
        );
    }
    for (label, total) in [
        ("当前路径合计", &usage.path_total),
        ("全会话合计", &usage.session_total),
    ] {
        let _ = writeln!(text, "{label}\t-\t-\t{}", render_usage_values(total));
    }
    text
}

fn render_usage_values(usage: &lattice_local::UsageSummary) -> String {
    let tokens = [
        &usage.input,
        &usage.cache_read,
        &usage.cache_write,
        &usage.output,
        &usage.reasoning,
    ]
    .map(|metric| render_metric(metric.known, metric.overflowed, |value| value.to_string()));
    let cost = render_metric(usage.cost.known, usage.cost.overflowed, |value| {
        let prefix = if value.exact { "" } else { "≈" };
        if value.usd > 0.0 && value.usd < 0.01 {
            format!("{prefix}<$0.01")
        } else {
            format!("{prefix}${:.2}", value.usd)
        }
    });
    format!("{}\t{}\t{cost}", usage.attempts, tokens.join("\t"))
}

fn render_metric<T: Copy>(
    known: Option<T>,
    overflowed: bool,
    format_value: impl FnOnce(T) -> String,
) -> String {
    let Some(known) = known else {
        return "-".into();
    };
    let mut value = format_value(known);
    if overflowed {
        value.push_str(" (overflow)");
    }
    value
}

pub(super) fn render_turn(turn: &lattice_local::TurnDetail) -> String {
    use std::fmt::Write;
    let mut text = format!(
        "turn {} session={} agent={}\nsummary={}\nusage={}\n",
        turn.turn_id,
        turn.session_id,
        turn.agent_name,
        serde_json::to_string(&turn.summary).expect("summary"),
        serde_json::to_string(&turn.usage).expect("usage")
    );
    for step in &turn.steps {
        let _ = writeln!(
            text,
            "step {} node={} progress={} end={} updated_at={}",
            step.key.ordinal,
            step.node.0,
            serde_json::to_string(&step.progress).expect("progress"),
            serde_json::to_string(&step.end).expect("end"),
            step.updated_at
        );
        for attempt in &step.attempts {
            let _ = writeln!(
                text,
                "  attempt {}: {}",
                attempt.attempt,
                serde_json::to_string(attempt).expect("attempt")
            );
        }
        for slot in &step.slots {
            let _ = writeln!(
                text,
                "  slot {}: {}",
                slot.invocation_id,
                serde_json::to_string(slot).expect("slot")
            );
        }
    }
    text
}

fn display_id(id: &str) -> String {
    if id.len() > 8 {
        let end = id.char_indices().nth(8).map(|(i, _)| i).unwrap_or(id.len());
        format!("{}..", &id[..end])
    } else {
        id.to_string()
    }
}

/// Truncate `text` to at most `max` display columns, appending `…` if cut.
///
/// NOTE: `lattice-agent/src/agent_loop/mod.rs::truncate_to_width` contains a
/// similar column-width truncation loop. Keep both in sync if this logic changes.
fn summarize(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.width() <= max {
        return one_line;
    }
    let mut width = 0_usize;
    let mut end = 0_usize;
    let ellipsis = '\u{2026}';
    let ellipsis_width = ellipsis.width().unwrap_or(1);
    let limit = max.saturating_sub(ellipsis_width);
    for (i, c) in one_line.char_indices() {
        let cw = c.width().unwrap_or(0);
        if width + cw > limit {
            break;
        }
        width += cw;
        end = i + c.len_utf8();
    }
    let mut out = one_line[..end].to_string();
    out.push(ellipsis);
    out
}

#[cfg(test)]
mod session_cli {
    use super::*;
    use clap::{CommandFactory, Parser};
    use lattice_local::{CostEstimate, UsageCompleteness};
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: SessionAction,
    }
    #[test]
    fn saturated_usage_is_not_presented_as_an_exact_total() {
        let mut usage = lattice_local::UsageSummary::from_attempts([]);
        usage.input = lattice_local::TokenSummary {
            known: Some(u64::MAX),
            complete: false,
            overflowed: true,
        };
        usage.kind = UsageCompleteness::Partial;
        let text = render_usage_values(&usage);
        let fields: Vec<_> = text.split('\t').collect();
        assert_eq!(fields[1], format!("{} (overflow)", u64::MAX));
        assert_eq!(fields[6], "$0.00");
    }

    #[test]
    fn complete_usage_rows_keep_exact_approximate_and_unknown_costs_distinct() {
        for (cost, expected) in [
            (Some((0.0, true)), "$0.00"),
            (Some((0.0, false)), "≈$0.00"),
            (Some((0.000001, true)), "<$0.01"),
            (Some((0.004, true)), "<$0.01"),
            (Some((0.004, false)), "≈<$0.01"),
            (Some((0.009999, true)), "<$0.01"),
            (Some((0.01, true)), "$0.01"),
            (Some((0.01, false)), "≈$0.01"),
            (Some((0.016, true)), "$0.02"),
            (None, "-"),
        ] {
            let usage = lattice_local::UsageSummary::from_attempts([(
                Some(lattice_local::TokenUsage {
                    input: Some(1),
                    ..Default::default()
                }),
                UsageCompleteness::Complete,
                cost.map(|(usd, exact)| CostEstimate { usd, exact }),
            )]);
            let text = render_usage_values(&usage);
            let fields: Vec<_> = text.split('\t').collect();
            assert_eq!(&fields[2..6], &["-"; 4]);
            assert_eq!(fields[6], expected, "{text}");
            assert_eq!(fields.len(), 7);
        }
    }

    #[test]
    fn partial_usage_keeps_unknown_cost_cell_unadorned() {
        let usage =
            lattice_local::UsageSummary::from_attempts([(None, UsageCompleteness::Partial, None)]);
        let text = render_usage_values(&usage);
        assert_eq!(text.split('\t').next_back(), Some("-"));
    }

    #[test]
    fn session_surface_and_numeric_nodes() {
        let command = TestCli::command();
        assert_eq!(
            command
                .get_subcommands()
                .map(|c| c.get_name())
                .collect::<Vec<_>>(),
            [
                "send",
                "feed",
                "context-op",
                "trigger",
                "inspect",
                "rewind",
                "list",
                "log",
                "usage",
                "turn",
                "cancel"
            ]
        );
        for n in ["0", "18446744073709551615"] {
            assert!(TestCli::try_parse_from(["session", "rewind", "abcd", n]).is_ok());
        }
        for n in ["-1", "+1", "aabb", "18446744073709551616"] {
            assert!(TestCli::try_parse_from(["session", "rewind", "abcd", n]).is_err());
        }
        assert!(TestCli::try_parse_from(["session", "send", "hello", "--agent", "coder"]).is_ok());
        assert!(TestCli::try_parse_from(["session", "send", "hello", "--session", "abcd"]).is_ok());
        assert!(TestCli::try_parse_from(["session", "send", "hello"]).is_err());
        assert!(TestCli::try_parse_from(["session", "log"]).is_err());
        assert!(TestCli::try_parse_from(["session", "rewind", "abcd"]).is_err());
        let id = "10000000-0000-4000-8000-000000000001";
        assert!(TestCli::try_parse_from(["session", "turn", id]).is_ok());
        assert!(TestCli::try_parse_from(["session", "cancel", "abcd"]).is_ok());
        assert!(
            TestCli::try_parse_from([
                "session",
                "send",
                "hello",
                "--agent",
                "coder",
                "--turn-id",
                id
            ])
            .is_err()
        );
        assert!(TestCli::try_parse_from(["session", "cancel", "abcd", "invalid"]).is_err());
        let mut command = TestCli::command();
        let help = command
            .find_subcommand_mut("cancel")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("取消会话当前正在执行的一轮"));
    }

    #[test]
    fn context_op_plan_validates_image_placement_dedup_and_image_spec() {
        let dir = tempfile::tempdir().unwrap();
        let img_a = dir.path().join("a.png");
        let img_a_copy = dir.path().join("a_copy.png");
        let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0x42; 32]);
        std::fs::write(&img_a, &png).unwrap();
        std::fs::write(&img_a_copy, &png).unwrap();
        let expected_ref = lattice_local::image_file_ref(1, &img_a).unwrap();

        let ch = parse_channel("cli").unwrap();
        // 空编辑列表拒绝。
        let empty_err = build_context_op_plan_parts(ch.clone(), &[]).unwrap_err();
        assert_eq!(empty_err.kind, lattice_local::LocalErrorKind::BadRequest);
        assert_eq!(empty_err.message, "至少需要指定一条上下文编辑操作");

        // --image 出现在首个 --replace-input 之前拒绝。
        let leading_img_err =
            build_context_op_plan_parts(ch.clone(), &[RawContextOpToken::Image(img_a.clone())])
                .unwrap_err();
        assert_eq!(
            leading_img_err.kind,
            lattice_local::LocalErrorKind::BadRequest
        );
        assert_eq!(
            leading_img_err.message,
            "--image 只能紧跟在 --replace-input 之后"
        );

        // --image 紧跟在非 --replace-input 编辑之后拒绝。
        let misplaced_img_err = build_context_op_plan_parts(
            ch.clone(),
            &[
                RawContextOpToken::OmitNode(lattice_local::NodeId(1)),
                RawContextOpToken::Image(img_a.clone()),
            ],
        )
        .unwrap_err();
        assert_eq!(
            misplaced_img_err.message,
            "--image 只能紧跟在 --replace-input 之后"
        );

        // 相同内容图片被多次引用时去重，且空文本 + --image 形成纯图片输入。
        let (op, unique_images) = build_context_op_plan_parts(
            ch.clone(),
            &[
                RawContextOpToken::ReplaceInput {
                    node: "1".into(),
                    text: "新文本".into(),
                },
                RawContextOpToken::Image(img_a.clone()),
                RawContextOpToken::ReplaceInput {
                    node: "2".into(),
                    text: String::new(),
                },
                RawContextOpToken::Image(img_a_copy.clone()),
            ],
        )
        .expect("含重复图片与纯图片的组合 context-op 应合法");
        assert_eq!(op.edits().len(), 2);
        assert_eq!(unique_images.len(), 1, "相同摘要的图片必须按 sha256 去重");
        assert_eq!(unique_images[0].0, expected_ref);
        match &op.edits()[1] {
            lattice_local::ContextEdit::ReplaceInput { node, message } => {
                assert_eq!(*node, lattice_local::NodeId(2));
                assert_eq!(message.as_message().parts.len(), 1);
                assert!(matches!(
                    message.as_message().parts[0].semantic,
                    lattice_local::MessageSemantic::Image { image } if image == expected_ref
                ));
            }
            other => panic!("预期为 ReplaceInput，实际为 {other:?}"),
        }

        // <IMAGE> 既支持 <media>:<len>:<sha256>（含大小写与 jpg 别名）也支持 @<PATH>。
        let formatted = format_image_ref(&expected_ref);
        assert_eq!(parse_image_spec(&formatted).unwrap(), expected_ref);
        assert_eq!(
            parse_image_spec(&formatted.to_ascii_uppercase().replacen("PNG:", "png:", 1)).unwrap(),
            expected_ref
        );
        assert_eq!(
            parse_image_spec(&format!("@{}", img_a.display())).unwrap(),
            expected_ref
        );
        let jpg_spec = format!("jpg:12:{}", "AB".repeat(32));
        assert_eq!(
            parse_image_spec(&jpg_spec).unwrap().media_type,
            lattice_local::ImageMediaType::Jpeg
        );
        for bad in [
            "bmp:12:0000000000000000000000000000000000000000000000000000000000000000",
            "png:0:0000000000000000000000000000000000000000000000000000000000000000",
            "png:12:short",
            "@",
        ] {
            assert!(
                parse_image_spec(bad).is_err(),
                "非法图片引用 `{bad}` 必须被拒绝"
            );
        }
    }

    #[test]
    fn render_inspect_formats_summary_channels_operated_nodes_images_and_entries() {
        let img = lattice_local::ImageRef {
            sha256: [0xab; 32],
            media_type: lattice_local::ImageMediaType::Jpeg,
            byte_length: 2048,
        };
        let mut channel_counts = std::collections::BTreeMap::new();
        channel_counts.insert(parse_channel("cli").unwrap(), 1);
        channel_counts.insert(parse_channel("tui").unwrap(), 2);
        let reply = lattice_local::InspectReply {
            session_id: "11111111111141118111111111111111".parse().unwrap(),
            position: lattice_local::NodeId(6),
            inspection: lattice_local::ContextInspection {
                entries: vec![
                    lattice_local::EffectiveEntrySummary::Input {
                        node: lattice_local::NodeId(1),
                        channel: parse_channel("cli").unwrap(),
                        text_bytes: 12,
                        image_count: 1,
                        image_bytes: 2048,
                    },
                    lattice_local::EffectiveEntrySummary::Step {
                        node: lattice_local::NodeId(2),
                        text_bytes: 30,
                        reasoning_bytes: 16,
                        tool_call_count: 1,
                        tool_result_bytes: 24,
                    },
                ],
                operated_nodes: vec![
                    lattice_local::OperatedNode {
                        node: lattice_local::NodeId(1),
                        status: lattice_local::NodeStatus::Modified,
                    },
                    lattice_local::OperatedNode {
                        node: lattice_local::NodeId(3),
                        status: lattice_local::NodeStatus::Omitted,
                    },
                    lattice_local::OperatedNode {
                        node: lattice_local::NodeId(4),
                        status: lattice_local::NodeStatus::SkippedEmpty,
                    },
                ],
                channel_counts,
                total_text_bytes: 82,
                images: vec![img],
                total_image_bytes: 2048,
            },
        };
        let text = render_inspect(&reply);
        assert!(
            text.contains("session 11111111111141118111111111111111 @6"),
            "{text}"
        );
        assert!(text.contains("通道计数：cli=1, tui=2"), "{text}");
        assert!(
            text.contains("已操作节点：@1 (已修改), @3 (已移出), @4 (已置空跳过)"),
            "{text}"
        );
        assert!(
            text.contains(&format!("jpeg:2048:{}", "ab".repeat(32))),
            "{text}"
        );
        assert!(
            text.contains("@1 input channel=cli 状态=已修改 文本=12 图片=1"),
            "{text}"
        );
        assert!(
            text.contains("@2 step 状态=原样 文本=30 推理=有 工具调用=1 工具结果=24"),
            "{text}"
        );
    }
    #[test]
    fn tree_output_keeps_root_branches_current_and_notice() {
        let log: lattice_local::SessionLog = serde_json::from_value(serde_json::json!({
            "session": {"session_id":"11111111111111111111111111111111", "project_path": b"/test-project".as_slice(), "current":4, "created_at":"2026-09-27T00:00:00Z", "updated_at":"2026-09-27T00:00:00Z"},
            "current_agent":"beta", "unclosed_turn":"20000000-0000-4000-8000-000000000002",
            "nodes":[
                {"node":{"id":0,"parent":null,"turn_id":null,"kind":{"kind":"root"}},"current":false,"turn_start":null},
                {"node":{"id":1,"parent":0,"turn_id":null,"kind":{"kind":"input"}},"current":false,"turn_start":null},
                {"node":{"id":2,"parent":1,"turn_id":"10000000-0000-4000-8000-000000000001","kind":{"kind":"step","ordinal":0}},"current":false,"turn_start":{"turn_id":"10000000-0000-4000-8000-000000000001","agent_name":"alpha","summary":{"outcome":"cancelled","progress":{"progress":"closed","end":{"end":"stop","cause":{"cause":"cancelled"}}},"notice":{"level":"info","text":"cancelled notice"}}}},
                {"node":{"id":3,"parent":0,"turn_id":null,"kind":{"kind":"input"}},"current":false,"turn_start":null},
                {"node":{"id":4,"parent":3,"turn_id":"20000000-0000-4000-8000-000000000002","kind":{"kind":"step","ordinal":0}},"current":true,"turn_start":{"turn_id":"20000000-0000-4000-8000-000000000002","agent_name":"beta","summary":{"outcome":"failed","progress":{"progress":"reserved"},"notice":{"level":"error","text":"process exited"}}}},
                {"node":{"id":5,"parent":2,"turn_id":null,"kind":{"kind":"context_op"}},"current":false,"turn_start":null}
            ]
        })).unwrap();
        let text = render_log(&log);
        assert!(text.contains("current=4 agent=beta"));
        assert!(text.contains("  0 root"));
        assert!(text.contains("    1 input"));
        assert!(text.contains("      2 step 0"));
        assert!(text.contains("        5 context_op"));
        assert!(text.contains("    3 input"));
        assert!(text.contains("    * 4 step 0"));
        assert!(text.contains("cancelled notice"));
        assert!(text.contains("alpha"));
    }
    #[test]
    fn turn_output_keeps_reserved_node_and_process_exit_conclusion() {
        let turn: lattice_local::TurnDetail = serde_json::from_value(serde_json::json!({
            "session_id":"11111111111141118111111111111111",
            "turn_id":"10000000-0000-4000-8000-000000000001",
            "start_node":2,"input_nodes":[1],"agent_name":"alpha",
            "accepted_at":"2026-09-27T00:00:00Z","ended_at":"2026-09-27T00:00:01Z",
            "summary":{"outcome":"failed","progress":{"progress":"reserved"},"notice":{"level":"error","text":"process exited"}},
            "usage":lattice_local::UsageSummary::from_attempts([]),
            "steps":[{"node":2,"key":{"turn_id":"10000000-0000-4000-8000-000000000001","ordinal":0},"progress":{"progress":"reserved"},"end":{"end":"stop","cause":{"cause":"process_exit"}},"attempts":[],"slots":[],"updated_at":"2026-09-27T00:00:01Z"}]
        })).unwrap();
        let text = render_turn(&turn);
        assert!(text.contains("step 0 node=2"), "{text}");
        assert!(text.contains("\"end\":\"stop\""), "{text}");
        assert!(text.contains("\"progress\":\"reserved\""), "{text}");
        assert!(text.contains("process_exit"), "{text}");
        assert!(text.contains("process exited"), "{text}");
    }

    #[test]
    fn turn_slot_output_uses_slot_state_reason_and_tool_identity_without_evidence() {
        let slot_json = serde_json::json!({
            "invocation_id": "10000000-0000-4000-8000-000000000001",
            "binding": {
                "session_id": "11111111111141118111111111111111",
                "turn_id": "10000000-0000-4000-8000-000000000001",
                "model_step_ordinal": 0,
                "part_ordinal": 0,
                "method": "observe__workspace",
                "tool_identity": "fake-observe-v1",
                "arguments_digest": "00".repeat(32)
            },
            "progress": {
                "state": "terminal",
                "outcome": "failed",
                "reason": {"kind": "failed", "value": {"code": "execution_failed"}},
                "material": {"kind": "no_content"}
            },
            "outcome": "failed",
            "reason": {"kind": "failed", "value": {"code": "execution_failed"}},
            "material": {"kind": "no_content"}
        });
        let mut legacy = slot_json.clone();
        legacy["evidence"] = serde_json::json!({"kind": "unavailable"});
        assert!(
            serde_json::from_value::<lattice_session::store::SlotDetail>(legacy).is_err(),
            "SlotDetail 必须拒绝旧 evidence 字段"
        );

        let turn: lattice_local::TurnDetail = serde_json::from_value(serde_json::json!({
            "session_id": "11111111111141118111111111111111",
            "turn_id": "10000000-0000-4000-8000-000000000001",
            "start_node": 2,
            "input_nodes": [1],
            "agent_name": "alpha",
            "accepted_at": "2026-09-27T00:00:00Z",
            "ended_at": "2026-09-27T00:00:01Z",
            "summary": {
                "outcome": "completed",
                "progress": {"progress": "closed", "end": {"end": "finish"}},
                "notice": null
            },
            "usage": lattice_local::UsageSummary::from_attempts([]),
            "steps": [{
                "node": 2,
                "key": {"turn_id": "10000000-0000-4000-8000-000000000001", "ordinal": 0},
                "progress": {"progress": "closed", "end": {"end": "finish"}},
                "end": {"end": "finish"},
                "attempts": [],
                "slots": [slot_json],
                "updated_at": "2026-09-27T00:00:01Z"
            }]
        }))
        .expect("解析包含新结构 SlotDetail 的 TurnDetail");

        let text = render_turn(&turn);
        assert!(
            text.contains("  slot 10000000-0000-4000-8000-000000000001: "),
            "{text}"
        );
        assert!(
            text.contains("\"tool_identity\":\"fake-observe-v1\""),
            "{text}"
        );
        assert!(text.contains("\"state\":\"terminal\""), "{text}");
        assert!(text.contains("\"code\":\"execution_failed\""), "{text}");
        assert!(!text.contains("evidence"), "{text}");
        assert!(!text.contains("descriptor_identity"), "{text}");

        let encoded = serde_json::to_value(&turn).expect("序列化 TurnDetail");
        assert_eq!(
            encoded["steps"][0]["slots"][0]["binding"]["tool_identity"],
            "fake-observe-v1"
        );
        assert!(encoded["steps"][0]["slots"][0].get("evidence").is_none());
    }

    #[test]
    fn usage_table_renders_start_node_at_prefix() {
        let usage: lattice_local::SessionUsage = serde_json::from_value(serde_json::json!({
            "session_id": "11111111111141118111111111111111",
            "position": 5,
            "turns": [{
                "turn_id": "10000000-0000-4000-8000-000000000001",
                "start_node": 2,
                "provider": "openai",
                "model": "gpt-test",
                "usage": lattice_local::UsageSummary::from_attempts([])
            }],
            "path_total": lattice_local::UsageSummary::from_attempts([]),
            "session_total": lattice_local::UsageSummary::from_attempts([])
        }))
        .expect("解析 SessionUsage");
        let text = render_usage(&usage);
        assert!(text.contains("NODE\tTURN\tPROVIDER/MODEL"), "{text}");
        assert!(
            text.contains("@2\t10000000-0000-4000-8000-000000000001\topenai/gpt-test\t"),
            "用量表首列应以 @<start_node> 标识轮次：{text}"
        );
    }
}
