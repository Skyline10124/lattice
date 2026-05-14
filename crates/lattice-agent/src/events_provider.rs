use crate::blob::StoredBlob;
use crate::bus_event_collector::{BusEventCollector, ContextEvent};
use async_trait::async_trait;
use lattice_core::tokens::TokenEstimator;
use lattice_prompt::{AssemblyContext, ContextProvider, Layer, PromptSection, TokenBudget};
use std::any::Any;
use std::sync::Arc;

const INLINE_THRESHOLD: u32 = 500;

pub struct EventsProvider {
    source: String,
    event_collector: Option<BusEventCollector>,
    blob_store: Option<Arc<crate::blob::BlobStore>>,
    priority: u8,
    budget: TokenBudget,
}

impl EventsProvider {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.to_string(),
            event_collector: None,
            blob_store: None,
            priority: 5,
            budget: TokenBudget::Dynamic,
        }
    }

    pub fn with_events(mut self, events: Vec<ContextEvent>) -> Self {
        let collector = BusEventCollector::new();
        collector.push_many(events);
        self.event_collector = Some(collector);
        self
    }

    pub fn with_event_collector(mut self, event_collector: BusEventCollector) -> Self {
        self.event_collector = Some(event_collector);
        self
    }

    pub fn with_blob_store(mut self, blob_store: Arc<crate::blob::BlobStore>) -> Self {
        self.blob_store = Some(blob_store);
        self
    }
}

fn format_inline(event: &ContextEvent) -> String {
    let payload_str =
        serde_json::to_string_pretty(&event.payload).unwrap_or_else(|_| event.payload.to_string());
    format!("[topic: {}] {}", event.topic, payload_str)
}

fn format_ref(blob: &StoredBlob) -> String {
    format!("[topic: {}] {} | {}B", blob.topic, blob.key, blob.size)
}

fn generate_key(source: &str, topic: &str, payload: &serde_json::Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let payload_str = serde_json::to_string(payload).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    payload_str.hash(&mut hasher);
    let hash = format!("{:x}", hasher.finish());
    format!("blob://{}/{}/{}", source, topic, hash)
}

#[async_trait]
impl ContextProvider for EventsProvider {
    fn configure_from_spec(&mut self, spec: &lattice_prompt::spec::PromptProviderSpec) {
        self.priority = spec.priority;
        self.budget = spec.budget.to_token_budget(512);
    }

    fn configure(&mut self, key: &'static str, value: &(dyn Any + Send + Sync)) {
        match key {
            "lattice_agent.bus_event_collector" => {
                if self.event_collector.is_none() {
                    if let Some(collector) = value.downcast_ref::<BusEventCollector>() {
                        self.event_collector = Some(collector.clone());
                    }
                }
            }
            "lattice_agent.blob_store" => {
                if let Some(blob_store) = value.downcast_ref::<Arc<crate::blob::BlobStore>>() {
                    self.blob_store = Some(Arc::clone(blob_store));
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "events"
    }

    fn layer(&self) -> Layer {
        Layer::Events
    }

    fn priority(&self) -> u8 {
        self.priority
    }

    fn budget(&self) -> TokenBudget {
        self.budget
    }

    async fn produce(&self, _ctx: &AssemblyContext<'_>) -> Option<PromptSection> {
        let events = self
            .event_collector
            .as_ref()
            .map(BusEventCollector::drain)
            .unwrap_or_default();
        if events.is_empty() {
            return None;
        }

        let mut inline_parts = Vec::new();
        let mut ref_parts = Vec::new();

        for event in &events {
            let payload_str = serde_json::to_string(&event.payload).unwrap_or_default();
            let tokens = TokenEstimator::estimate_text(&payload_str);

            if tokens <= INLINE_THRESHOLD {
                inline_parts.push(format_inline(event));
            } else if let Some(blob_store) = self.blob_store.as_deref() {
                let key = generate_key(&self.source, &event.topic, &event.payload);
                let payload_str = serde_json::to_string_pretty(&event.payload)
                    .unwrap_or_else(|_| event.payload.to_string());
                let blob = StoredBlob {
                    key: key.clone(),
                    source: event.source.clone(),
                    topic: event.topic.clone(),
                    mime: "application/json".to_string(),
                    size: payload_str.len() as u64,
                    payload: payload_str,
                    summary: String::new(),
                };
                if blob_store.insert(&blob).await.is_ok() {
                    ref_parts.push(format_ref(&blob));
                }
                // insert failed → skip event silently
            }
            // no blob_store + over threshold → skip event
        }

        if inline_parts.is_empty() && ref_parts.is_empty() {
            return None;
        }

        let mut content_parts = inline_parts;
        content_parts.extend(ref_parts);
        let content = content_parts.join("\n");
        let tokens = TokenEstimator::estimate_text(&content);

        Some(PromptSection {
            content,
            layer: Layer::Events,
            priority: self.priority,
            tokens,
        })
    }
}

/// Factory for EventsProvider. Registered in AgentFactory::assemble().
pub struct EventsFactory;

impl lattice_prompt::ProviderFactory for EventsFactory {
    fn kind(&self) -> lattice_prompt::spec::ProviderKind {
        lattice_prompt::spec::ProviderKind::Events
    }

    fn create(
        &self,
        spec: &lattice_prompt::spec::PromptProviderSpec,
    ) -> Box<dyn lattice_prompt::ContextProvider> {
        let mut provider = EventsProvider::new(&spec.id);
        provider.configure_from_spec(spec);
        Box::new(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::BlobStore;
    use lattice_core::catalog::ApiProtocol;
    use lattice_core::ResolvedModel;
    use lattice_prompt::AssemblyContext;
    use lattice_prompt::ProviderFactory;
    use std::collections::HashMap;

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test".into(),
            api_model_id: "test".into(),
            provider: "test".into(),
            base_url: "http://localhost".to_string(),
            api_key: Some("sk-test".into()),
            api_protocol: ApiProtocol::OpenAiChat,
            context_length: 16384,
            credential_status: lattice_core::CredentialStatus::Present,
            provider_specific: HashMap::new(),
        }
    }

    fn make_event(topic: &str, payload: serde_json::Value) -> ContextEvent {
        ContextEvent::new(topic, "test-source", payload)
    }

    #[tokio::test]
    async fn test_inline_small_event() {
        let store = Arc::new(BlobStore::connect("sqlite::memory:").await.unwrap());
        let resolved = make_resolved();
        let small_event = make_event("audit", serde_json::json!({"result": "pass"}));
        let provider = EventsProvider::new("test-source")
            .with_blob_store(store)
            .with_events(vec![small_event]);
        let ctx = AssemblyContext {
            request_id: "test",
            model: &resolved,
            user_input: "test",
        };
        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("[topic: audit]"));
        assert!(section.content.contains("\"result\": \"pass\""));
        assert_eq!(section.layer, Layer::Events);
    }

    #[tokio::test]
    async fn test_blob_large_event() {
        let store = Arc::new(BlobStore::connect("sqlite::memory:").await.unwrap());
        let resolved = make_resolved();
        // Create a payload > 500 tokens (lots of repeated text to exceed char/4 threshold)
        let large_payload = serde_json::json!({
            "lines": vec![String::from("this is a test line that will be repeated many times to exceed the threshold").repeat(100)]
        });
        let large_event = make_event("inspection", large_payload);
        let provider = EventsProvider::new("test-source")
            .with_blob_store(Arc::clone(&store))
            .with_events(vec![large_event]);
        let ctx = AssemblyContext {
            request_id: "test",
            model: &resolved,
            user_input: "test",
        };
        let section = provider.produce(&ctx).await.unwrap();
        // Should contain a blob reference, not inline content
        assert!(section.content.contains("blob://test-source/inspection/"));
        // Should NOT contain the raw payload inline
        assert!(!section.content.contains("this is a test line"));
    }

    #[tokio::test]
    async fn test_skip_large_event_without_blob_store() {
        let resolved = make_resolved();
        let large_payload = serde_json::json!({
            "lines": vec![String::from("x".repeat(5000))]
        });
        let large_event = make_event("skip-me", large_payload);
        let provider = EventsProvider::new("test-source").with_events(vec![large_event]);
        let ctx = AssemblyContext {
            request_id: "test",
            model: &resolved,
            user_input: "test",
        };
        // Over threshold with no blob_store → all events skipped → None
        assert!(provider.produce(&ctx).await.is_none());
    }

    #[tokio::test]
    async fn test_no_bus_events_returns_none() {
        let provider = EventsProvider::new("test-source");
        let resolved = make_resolved();
        let ctx = AssemblyContext {
            request_id: "test",
            model: &resolved,
            user_input: "test",
        };
        assert!(provider.produce(&ctx).await.is_none());
    }

    #[tokio::test]
    async fn test_mixed_inline_and_blob() {
        let store = Arc::new(BlobStore::connect("sqlite::memory:").await.unwrap());
        let resolved = make_resolved();
        let small = make_event("small", serde_json::json!({"ok": true}));
        let large = make_event("big", serde_json::json!({"data": "x".repeat(5000)}));
        let provider = EventsProvider::new("test-source")
            .with_blob_store(store)
            .with_events(vec![small, large]);
        let ctx = AssemblyContext {
            request_id: "test",
            model: &resolved,
            user_input: "test",
        };
        let section = provider.produce(&ctx).await.unwrap();
        // Small event inline
        assert!(section.content.contains("[topic: small]"));
        // Large event as blob reference
        assert!(section.content.contains("blob://test-source/big/"));
    }

    #[test]
    fn events_factory_kind() {
        let factory = EventsFactory;
        assert!(matches!(
            factory.kind(),
            lattice_prompt::spec::ProviderKind::Events
        ));
    }

    #[test]
    fn events_factory_create() {
        let factory = EventsFactory;
        let spec = lattice_prompt::spec::PromptProviderSpec {
            id: "test-events".to_string(),
            kind: lattice_prompt::spec::ProviderKind::Events,
            layer: lattice_prompt::Layer::Events,
            channel: None,
            priority: 5,
            cache_scope: None,
            budget: lattice_prompt::spec::BudgetSpec::Dynamic,
            limit: None,
        };
        let provider = factory.create(&spec);
        assert_eq!(provider.layer(), lattice_prompt::Layer::Events);
    }
}
