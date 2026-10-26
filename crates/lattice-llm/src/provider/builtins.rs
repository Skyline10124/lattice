// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use crate::protocol::{
    Cache, ChatCompletionsFeatures, EffortParam, Features, MessagesFeatures, ReasoningReplay,
    ThinkingBudget, ThinkingToggle,
};

use super::{Auth, Provider};

const CHAT: ChatCompletionsFeatures = ChatCompletionsFeatures {
    effort_param: EffortParam::ReasoningEffort,
    thinking_toggle: ThinkingToggle::None,
    thinking_budget: ThinkingBudget::None,
    reasoning_replay: ReasoningReplay::None,
    cache_markers: false,
};
const STANDARD: Features = Features::ChatCompletions(CHAT);
const THINKING_TYPE: Features = Features::ChatCompletions(ChatCompletionsFeatures {
    thinking_toggle: ThinkingToggle::ThinkingType,
    reasoning_replay: ReasoningReplay::ReasoningContent,
    ..CHAT
});
const THINKING_BUDGET: ChatCompletionsFeatures = ChatCompletionsFeatures {
    thinking_toggle: ThinkingToggle::EnableThinking,
    thinking_budget: ThinkingBudget::ThinkingBudget,
    ..CHAT
};

pub(super) fn providers() -> Vec<Provider> {
    use Auth::{Bearer, None as NoAuth, OauthChatgpt, XApiKey, XGoogApiKey};

    // #315 L5 合并前，openai、xai 暂用 chat-completions，google 暂用 generate-content；L5 承接切回合同协议。
    vec![
        provider(
            "openai",
            "https://api.openai.com/v1",
            Bearer,
            Some("openai"),
            STANDARD,
        ),
        provider(
            "chatgpt",
            "https://api.openai.com/v1",
            OauthChatgpt,
            Some("openai"),
            Features::Responses,
        ),
        provider(
            "anthropic",
            "https://api.anthropic.com",
            XApiKey,
            Some("anthropic"),
            Features::Messages(MessagesFeatures {
                cache: Cache::Automatic,
            }),
        ),
        provider(
            "google",
            "https://generativelanguage.googleapis.com/v1beta",
            XGoogApiKey,
            Some("google"),
            Features::GenerateContent,
        ),
        provider("xai", "https://api.x.ai/v1", Bearer, Some("xai"), STANDARD),
        provider(
            "mistral",
            "https://api.mistral.ai/v1",
            Bearer,
            Some("mistral"),
            Features::ChatCompletions(ChatCompletionsFeatures {
                reasoning_replay: ReasoningReplay::ContentChunks,
                ..CHAT
            }),
        ),
        provider(
            "deepseek",
            "https://api.deepseek.com",
            Bearer,
            Some("deepseek"),
            THINKING_TYPE,
        ),
        provider(
            "moonshotai",
            "https://api.moonshot.ai/v1",
            Bearer,
            Some("moonshotai"),
            THINKING_TYPE,
        ),
        provider(
            "moonshotai-cn",
            "https://api.moonshot.cn/v1",
            Bearer,
            Some("moonshotai-cn"),
            THINKING_TYPE,
        ),
        provider(
            "zai",
            "https://api.z.ai/api/paas/v4",
            Bearer,
            Some("zai"),
            THINKING_TYPE,
        ),
        provider(
            "zhipuai",
            "https://open.bigmodel.cn/api/paas/v4",
            Bearer,
            Some("zhipuai"),
            THINKING_TYPE,
        ),
        provider(
            "alibaba",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            Bearer,
            Some("alibaba"),
            Features::ChatCompletions(ChatCompletionsFeatures {
                cache_markers: true,
                ..THINKING_BUDGET
            }),
        ),
        provider(
            "alibaba-cn",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            Bearer,
            Some("alibaba-cn"),
            Features::ChatCompletions(ChatCompletionsFeatures {
                cache_markers: true,
                ..THINKING_BUDGET
            }),
        ),
        provider(
            "minimax",
            "https://api.minimax.io/anthropic",
            XApiKey,
            Some("minimax"),
            Features::Messages(MessagesFeatures { cache: Cache::None }),
        ),
        provider(
            "minimax-cn",
            "https://api.minimax.cn/anthropic",
            XApiKey,
            Some("minimax-cn"),
            Features::Messages(MessagesFeatures { cache: Cache::None }),
        ),
        provider(
            "volcengine",
            "https://ark.cn-beijing.volces.com/api/v3",
            Bearer,
            Some("volcengine"),
            Features::ChatCompletions(ChatCompletionsFeatures {
                thinking_toggle: ThinkingToggle::ThinkingType,
                ..CHAT
            }),
        ),
        provider(
            "siliconflow-cn",
            "https://api.siliconflow.cn/v1",
            Bearer,
            Some("siliconflow-cn"),
            Features::ChatCompletions(THINKING_BUDGET),
        ),
        provider(
            "openrouter",
            "https://openrouter.ai/api/v1",
            Bearer,
            Some("openrouter"),
            Features::ChatCompletions(ChatCompletionsFeatures {
                effort_param: EffortParam::ReasoningObject,
                thinking_budget: ThinkingBudget::ReasoningMaxTokens,
                reasoning_replay: ReasoningReplay::ReasoningContent,
                ..CHAT
            }),
        ),
        provider(
            "groq",
            "https://api.groq.com/openai/v1",
            Bearer,
            Some("groq"),
            STANDARD,
        ),
        provider(
            "ollama",
            "http://127.0.0.1:11434/v1",
            NoAuth,
            None,
            STANDARD,
        ),
        provider(
            "lmstudio",
            "http://127.0.0.1:1234/v1",
            NoAuth,
            Some("lmstudio"),
            STANDARD,
        ),
    ]
}

fn provider(
    id: &str,
    base_url: &str,
    auth: Auth,
    catalog: Option<&str>,
    features: Features,
) -> Provider {
    Provider {
        id: id.to_owned(),
        protocol: features.protocol(),
        base_url: base_url.to_owned(),
        auth,
        catalog: catalog.map(str::to_owned),
        features,
        models: BTreeMap::new(),
    }
}
