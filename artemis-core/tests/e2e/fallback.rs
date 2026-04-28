use artemis_core::agent_loop::{AgentLoop, LoopConfig, LoopEvent};
use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::errors::ErrorClassifier;
use artemis_core::mock::MockProvider;
use artemis_core::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use artemis_core::retry::RetryPolicy;
use artemis_core::types::{Message, Role};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

fn make_resolved(provider: &str, model: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: model.to_string(),
        provider: provider.to_string(),
        api_key: Some("sk-test".to_string()),
        base_url: "http://localhost".to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: model.to_string(),
        context_length: 131072,
        provider_specific: HashMap::new(),
    }
}

fn user_message(content: &str) -> Message {
    Message {
        role: Role::User,
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

struct FailingProvider {
    name: String,
    fail_count: Mutex<u32>,
}

impl FailingProvider {
    fn new(name: &str, fail_count: u32) -> Self {
        Self {
            name: name.to_string(),
            fail_count: Mutex::new(fail_count),
        }
    }
}

use async_trait::async_trait;

#[async_trait]
impl Provider for FailingProvider {
    async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let mut count = self.fail_count.lock().unwrap();
        if *count > 0 {
            *count -= 1;
            return Err(ProviderError::Api("provider temporarily down".to_string()));
        }
        Ok(ChatResponse {
            content: Some("Fallback succeeded!".to_string()),
            tool_calls: None,
            usage: None,
            finish_reason: "stop".to_string(),
            model: self.name.clone(),
        })
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<artemis_core::streaming::EventStream, ProviderError> {
        Err(ProviderError::Stream("not supported".to_string()))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_tools(&self) -> bool {
        true
    }
}

#[test]
fn test_fallback_primary_fails_secondary_succeeds() {
    let resolved = make_resolved("mock", "test-model");

    let failing = FailingProvider::new("primary", 3);
    let mut fallback = MockProvider::new("fallback");
    fallback.set_response("Fallback succeeded!");

    let agent = AgentLoop::new();
    let messages = vec![user_message("Hello")];

    let providers: Vec<&dyn Provider> = vec![&failing, &fallback];
    let policy = RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(10),
    };
    let classifier = ErrorClassifier;

    let events = agent.run_with_fallback(
        providers,
        resolved,
        messages,
        vec![],
        LoopConfig::default(),
        &classifier,
        &policy,
    );

    let has_done = events.iter().any(|e| matches!(e, LoopEvent::Done { .. }));
    assert!(has_done, "fallback provider should produce Done event");
}

#[test]
fn test_fallback_all_providers_fail() {
    let resolved = make_resolved("mock", "test-model");

    let failing1 = FailingProvider::new("primary", 10);
    let failing2 = FailingProvider::new("secondary", 10);

    let agent = AgentLoop::new();
    let messages = vec![user_message("Hello")];

    let providers: Vec<&dyn Provider> = vec![&failing1, &failing2];
    let policy = RetryPolicy {
        max_retries: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    };
    let classifier = ErrorClassifier;

    let events = agent.run_with_fallback(
        providers,
        resolved,
        messages,
        vec![],
        LoopConfig::default(),
        &classifier,
        &policy,
    );

    let has_error = events.iter().any(|e| matches!(e, LoopEvent::Error { .. }));
    assert!(
        has_error,
        "all providers failing should produce Error event"
    );
}

#[test]
fn test_router_priority_fallback_no_credentials() {
    use artemis_core::router::ModelRouter;
    use std::env;
    use std::sync::{LazyLock, Mutex as StdMutex};

    static LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));
    let _lock = LOCK.lock().unwrap();

    let prev_keys: Vec<(String, Option<String>)> = [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
    ]
    .iter()
    .map(|k| (k.to_string(), env::var(k).ok()))
    .collect();

    for k in &[
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
    ] {
        env::remove_var(k);
    }

    let router = ModelRouter::new();
    let result = router.resolve("claude-sonnet-4-6", None);
    assert!(result.is_ok(), "should resolve even without credentials");
    let resolved = result.unwrap();
    assert!(
        resolved.api_key.is_none(),
        "api_key should be None without env credentials"
    );

    for (k, v) in prev_keys {
        match v {
            Some(val) => env::set_var(&k, val),
            None => env::remove_var(&k),
        }
    }
}
