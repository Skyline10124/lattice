// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 模型步骤请求的组装：读取父链历史、转换冻结工具目录并构造步骤请求。

use lattice_agent::ModelStepRequest;
use lattice_conversation::{BoundToolCall, ConversationItem, NativeMessage, SessionId};
use lattice_llm::ChatRequest;
use lattice_session::store::*;

pub(super) fn history(
    read: &SessionRead,
    session_id: SessionId,
) -> lattice_session::store::Result<Vec<ConversationItem>> {
    let mut current = read.session(session_id)?.current;
    let mut items = Vec::new();
    while current != NodeId(0) {
        let node = read.node(NodeRef {
            session_id,
            node: current,
        })?;
        let content = node.step_content();
        match node {
            Node::UserMessage {
                parent, message, ..
            } => {
                items.push(ConversationItem::Message(message));
                current = parent;
            }
            Node::Step { parent, .. } => {
                if let Some(content) = content {
                    items.push(ConversationItem::Step(content));
                }
                current = parent;
            }
            Node::Root => {
                return Err(StoreError::Corruption {
                    table: "nodes".into(),
                    key: current.0.to_string(),
                    detail: "nonzero root".into(),
                });
            }
        }
    }
    items.reverse();
    Ok(items)
}

fn tool_definitions(config: &FrozenTurnConfig) -> Vec<lattice_llm::ToolDefinition> {
    config
        .catalog
        .entries
        .iter()
        .map(|tool| {
            lattice_llm::ToolDefinition::new(
                tool.name.clone(),
                tool.description.clone(),
                tool.input_schema.clone(),
            )
        })
        .collect()
}

/// 组装一个模型步骤的请求；`cancel` 与 `deadline` 留空，由发送方在每次尝试前设置。
pub(super) fn step_request(
    session: SessionId,
    config: &FrozenTurnConfig,
    history: Vec<ConversationItem>,
) -> ModelStepRequest {
    ModelStepRequest {
        system_prompt: config.system_prompt.clone(),
        history,
        model: config.model.clone(),
        provider: config.provider.clone(),
        effort: config.effort,
        tools: tool_definitions(config),
        cancel: None,
        deadline: None,
        session_id: Some(session.to_string()),
    }
}

/// 本轮首步请求的预览：当前父链历史加上尚未写入的用户消息，与接受后首步实际发送的请求相同。
pub(super) fn first_step_chat_request(
    read: &SessionRead,
    session: SessionId,
    config: &FrozenTurnConfig,
    message: &NativeMessage<BoundToolCall>,
) -> lattice_session::store::Result<ChatRequest> {
    let mut items = history(read, session)?;
    items.push(ConversationItem::Message(message.clone()));
    Ok(step_request(session, config, items).chat_request())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_request_preserves_system_history_tools_and_target() {
        let session =
            SessionId::parse("00000000000000000000000000000001").expect("解析测试会话标识");
        let provider =
            lattice_conversation::ProviderId::parse("openai").expect("解析测试 provider 标识");
        let model = lattice_conversation::ModelId::parse("gpt-4o").expect("解析测试模型标识");
        let effort = Some(lattice_conversation::Effort::Medium);
        let system_prompt = Some("保持简洁并遵守规则。".to_string());
        let first_schema = serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        });
        let second_schema = serde_json::json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"]
        });
        let config = FrozenTurnConfig {
            agent_name: lattice_session::AgentName::parse("coder").expect("解析测试 Agent 名"),
            model: model.clone(),
            provider: provider.clone(),
            effort,
            system_prompt: system_prompt.clone(),
            catalog: ToolCatalogSnapshot::new(vec![
                ToolCatalogEntry {
                    name: "alpha__read".to_string(),
                    tool_identity: "test:alpha__read@1".to_string(),
                    description: "读取文件".to_string(),
                    input_schema: first_schema.clone(),
                },
                ToolCatalogEntry {
                    name: "beta__search".to_string(),
                    tool_identity: "test:beta__search@1".to_string(),
                    description: "搜索代码".to_string(),
                    input_schema: second_schema.clone(),
                },
            ])
            .expect("构造测试工具目录快照"),
            limits: TurnExecutionLimits {
                timeout_secs: lattice_session::TurnTimeout::default(),
                max_model_steps: MAX_MODEL_STEPS,
                max_model_attempts: 3,
            },
        };
        let first_user = ConversationItem::Message(lattice_conversation::NativeMessage::new(
            lattice_conversation::MessageRole::User,
            None,
            vec![lattice_conversation::MessagePart::semantic_only(
                lattice_conversation::MessageSemantic::Text {
                    text: "请检查工作区".to_string(),
                },
            )],
        ));
        let first_step = ConversationItem::Step(lattice_conversation::StepContent {
            assistant: lattice_conversation::NativeMessage::new(
                lattice_conversation::MessageRole::Assistant,
                Some(lattice_conversation::ReplayOrigin {
                    protocol: lattice_conversation::Protocol::ChatCompletions,
                    provider_id: provider.clone(),
                }),
                vec![lattice_conversation::MessagePart::semantic_only(
                    lattice_conversation::MessageSemantic::Text {
                        text: "好的，先查看目录。".to_string(),
                    },
                )],
            ),
            results: Vec::new(),
        });
        let second_user = ConversationItem::Message(lattice_conversation::NativeMessage::new(
            lattice_conversation::MessageRole::User,
            None,
            vec![lattice_conversation::MessagePart::semantic_only(
                lattice_conversation::MessageSemantic::Text {
                    text: "继续下一步".to_string(),
                },
            )],
        ));
        let history = vec![first_user.clone(), first_step.clone(), second_user.clone()];

        let request = step_request(session, &config, history.clone());

        let expected_tools = vec![
            lattice_llm::ToolDefinition::new("alpha__read", "读取文件", first_schema),
            lattice_llm::ToolDefinition::new("beta__search", "搜索代码", second_schema),
        ];
        assert_eq!(request.system_prompt, system_prompt, "系统提示应原样保留");
        assert_eq!(request.history, history, "历史列表应按原顺序保留");
        assert_eq!(request.model, model, "模型标识应与冻结配置一致");
        assert_eq!(request.provider, provider, "provider 标识应与冻结配置一致");
        assert_eq!(request.effort, effort, "推理档位应与冻结配置一致");
        assert_eq!(request.tools, expected_tools, "工具定义应按目录顺序转换");
        assert!(request.cancel.is_none(), "步骤请求构造时 cancel 必须为空");
        assert!(
            request.deadline.is_none(),
            "步骤请求构造时 deadline 必须为空"
        );
        assert_eq!(
            request.session_id.as_deref(),
            Some("00000000000000000000000000000001"),
            "session_id 应写入规范会话标识"
        );

        let expected_system = ConversationItem::Message(lattice_conversation::NativeMessage::new(
            lattice_conversation::MessageRole::System,
            None,
            vec![lattice_conversation::MessagePart::semantic_only(
                lattice_conversation::MessageSemantic::Text {
                    text: "保持简洁并遵守规则。".to_string(),
                },
            )],
        ));
        let expected_chat = lattice_llm::ChatRequest {
            messages: vec![expected_system, first_user, first_step, second_user],
            tools: expected_tools,
            target: lattice_llm::ModelTarget {
                model: lattice_conversation::ModelRef::new(provider, model),
                effort,
            },
            client_request_id: Some("00000000000000000000000000000001".to_string()),
        };
        assert_eq!(
            request.chat_request(),
            expected_chat,
            "由步骤请求生成的 ChatRequest 必须与逐字段期望一致"
        );
    }

    #[test]
    fn preview_request_equals_first_persisted_request() {
        let state = tempfile::tempdir().expect("创建临时状态目录");
        let project = tempfile::tempdir().expect("创建临时项目目录");
        let canonical_project = std::fs::canonicalize(project.path()).expect("规范化项目路径");
        let layout = lattice_session::StateLayout::in_state_root(state.path());
        let store = SessionStore::create(&layout).expect("创建测试会话库");
        let session =
            SessionId::parse("00000000000000000000000000000002").expect("解析测试会话标识");
        let provider =
            lattice_conversation::ProviderId::parse("openai").expect("解析测试 provider 标识");
        let model = lattice_conversation::ModelId::parse("gpt-4o").expect("解析测试模型标识");
        let project_path =
            lattice_session::ProjectPath::from_path(&canonical_project).expect("合法规范项目路径");

        let config = FrozenTurnConfig {
            agent_name: lattice_session::AgentName::parse("reviewer").expect("解析测试 Agent 名"),
            model,
            provider: provider.clone(),
            effort: Some(lattice_conversation::Effort::High),
            system_prompt: Some("先核对图片细节，再调用只读工具。".to_string()),
            catalog: ToolCatalogSnapshot::new(vec![ToolCatalogEntry {
                name: "observe__workspace".to_string(),
                tool_identity: "test:observe__workspace@1".to_string(),
                description: "读取工作区文件".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            }])
            .expect("构造测试工具目录快照"),
            limits: TurnExecutionLimits {
                timeout_secs: lattice_session::TurnTimeout::default(),
                max_model_steps: MAX_MODEL_STEPS,
                max_model_attempts: 3,
            },
        };
        let settings = SessionSettings::for_test(&config);
        let settings_version = settings.version().expect("计算设置版本");

        let lock = store.try_lock_session(session).expect("获取会话执行锁");
        store
            .create_session(&lock, project_path.clone(), settings, chrono::Utc::now())
            .expect("创建测试会话");

        // 先完成一轮历史对话，验证预览与首步实际请求在非空历史链上同样一致。
        let first_turn =
            lattice_conversation::TurnId::parse("00000000-0000-4000-8000-000000000011")
                .expect("解析首轮 TurnId");
        store
            .accept_turn(
                &lock,
                AcceptTurn {
                    session_id: session,
                    turn_id: first_turn,
                    project_path: project_path.clone(),
                    input: TurnInput::text("先看上一轮文本".into()).expect("构造首轮文本输入"),
                    config: config.clone(),
                    accepted_at: chrono::Utc::now(),
                    settings_version,
                },
            )
            .expect("接受首轮测试请求");
        let first_key = lattice_conversation::ModelAttemptKey {
            turn_id: first_turn,
            model_step_ordinal: 0,
            attempt_ordinal: 0,
        };
        let origin = AttemptOrigin {
            protocol: AttemptProtocol::ChatCompletions,
            provider_id: provider.clone(),
        };
        store
            .start_request(&lock, first_key, origin.clone(), chrono::Utc::now())
            .expect("记录首轮模型尝试开始");
        store
            .close_step(
                &lock,
                CloseStep::Finish(CompleteReply {
                    key: first_key,
                    origin,
                    usage: None,
                    usage_kind: lattice_conversation::UsageCompleteness::Partial,
                    cost: None,
                    message: NativeMessage::new(
                        lattice_conversation::MessageRole::Assistant,
                        Some(lattice_conversation::ReplayOrigin {
                            protocol: lattice_conversation::Protocol::ChatCompletions,
                            provider_id: provider,
                        }),
                        vec![lattice_conversation::MessagePart::semantic_only(
                            lattice_conversation::MessageSemantic::Text {
                                text: "首轮已回复".to_string(),
                            },
                        )],
                    ),
                }),
                chrono::Utc::now(),
            )
            .expect("关闭首轮步骤");

        // 第二轮提交图文交错的用户消息，比较接受前预览与接受后首步实际请求。
        let png = b"\x89PNG\r\n\x1a\npreview-png".to_vec();
        let jpeg = b"\xff\xd8\xff\xe0preview-jpeg".to_vec();
        let second_input = TurnInput::new(vec![
            TurnInputPart::Text {
                text: "第一段文字".into(),
            },
            TurnInputPart::Image { bytes: png },
            TurnInputPart::Text {
                text: "中间文字".into(),
            },
            TurnInputPart::Image { bytes: jpeg },
            TurnInputPart::Text {
                text: "末尾文字".into(),
            },
        ])
        .expect("构造图文交错输入");

        let preview = {
            let read = store.begin_read().expect("打开接受前读事务");
            first_step_chat_request(&read, session, &config, second_input.message())
                .expect("构造接受前首步预览请求")
        };

        let second_turn =
            lattice_conversation::TurnId::parse("00000000-0000-4000-8000-000000000012")
                .expect("解析第二轮 TurnId");
        store
            .accept_turn(
                &lock,
                AcceptTurn {
                    session_id: session,
                    turn_id: second_turn,
                    project_path,
                    input: second_input,
                    config,
                    accepted_at: chrono::Utc::now(),
                    settings_version,
                },
            )
            .expect("接受第二轮测试请求");

        let persisted = {
            let read = store.begin_read().expect("打开接受后读事务");
            let persisted_config = read
                .turn_config(session, second_turn)
                .expect("读取已持久化的冻结配置");
            let items = history(&read, session).expect("读取接受后的父链历史");
            step_request(session, &persisted_config, items).chat_request()
        };

        assert_eq!(
            preview, persisted,
            "接受前由 first_step_chat_request 生成的预览必须与接受后首步实际请求完全相等"
        );
    }
}
