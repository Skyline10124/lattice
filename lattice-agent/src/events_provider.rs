use crate::blob::StoredBlob;
use crate::bus_event_collector::ContextEvent;
use crate::prompt::provider::ContextProvider;
use crate::prompt::types::{AssemblyContext, Layer, PromptSection, TokenBudget};
use async_trait::async_trait;
use lattice_core::tokens::TokenEstimator;

const INLINE_THRESHOLD: u32 = 500;

pub struct EventsProvider {
    source: String,
}

impl EventsProvider {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.to_string(),
        }
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
    fn layer(&self) -> Layer {
        Layer::Events
    }

    fn priority(&self) -> u8 {
        5
    }

    fn budget(&self) -> TokenBudget {
        TokenBudget::Dynamic
    }

    async fn produce(&self, ctx: &AssemblyContext<'_>) -> Option<PromptSection> {
        if ctx.bus_events.is_empty() {
            return None;
        }

        let mut inline_parts = Vec::new();
        let mut ref_parts = Vec::new();

        for event in ctx.bus_events {
            let payload_str = serde_json::to_string(&event.payload).unwrap_or_default();
            let tokens = TokenEstimator::estimate_text(&payload_str);

            if tokens <= INLINE_THRESHOLD {
                inline_parts.push(format_inline(event));
            } else if let Some(blob_store) = ctx.blob_store {
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
            priority: 5,
            tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::BlobStore;
    use crate::prompt::types::AssemblyContext;
    use lattice_core::catalog::ApiProtocol;
    use lattice_core::ResolvedModel;
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
        let provider = EventsProvider::new("test-source");
        let resolved = make_resolved();
        let small_event = make_event("audit", serde_json::json!({"result": "pass"}));
        let bus_events = vec![small_event];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            model: &resolved,
            user_input: "test",
            blob_store: Some(&BlobStore::connect("sqlite::memory:").await.unwrap()),
            bus_events: &bus_events,
        };
        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("[topic: audit]"));
        assert!(section.content.contains("\"result\": \"pass\""));
        assert_eq!(section.layer, Layer::Events);
    }

    #[tokio::test]
    async fn test_blob_large_event() {
        let store = BlobStore::connect("sqlite::memory:").await.unwrap();
        let provider = EventsProvider::new("test-source");
        let resolved = make_resolved();
        // Create a payload > 500 tokens (lots of repeated text to exceed char/4 threshold)
        let large_payload = serde_json::json!({
            "lines": vec![String::from("this is a test line that will be repeated many times to exceed the threshold").repeat(100)]
        });
        let large_event = make_event("inspection", large_payload);
        let bus_events = vec![large_event];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            model: &resolved,
            user_input: "test",
            blob_store: Some(&store),
            bus_events: &bus_events,
        };
        let section = provider.produce(&ctx).await.unwrap();
        // Should contain a blob reference, not inline content
        assert!(section.content.contains("blob://test-source/inspection/"));
        // Should NOT contain the raw payload inline
        assert!(!section.content.contains("this is a test line"));
    }

    #[tokio::test]
    async fn test_skip_large_event_without_blob_store() {
        let provider = EventsProvider::new("test-source");
        let resolved = make_resolved();
        let large_payload = serde_json::json!({
            "lines": vec![String::from("x".repeat(5000))]
        });
        let large_event = make_event("skip-me", large_payload);
        let bus_events = vec![large_event];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            model: &resolved,
            user_input: "test",
            blob_store: None,
            bus_events: &bus_events,
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
            memory: None,
            model: &resolved,
            user_input: "test",
            blob_store: None,
            bus_events: &[],
        };
        assert!(provider.produce(&ctx).await.is_none());
    }

    #[tokio::test]
    async fn test_mixed_inline_and_blob() {
        let store = BlobStore::connect("sqlite::memory:").await.unwrap();
        let provider = EventsProvider::new("test-source");
        let resolved = make_resolved();
        let small = make_event("small", serde_json::json!({"ok": true}));
        let large = make_event("big", serde_json::json!({"data": "x".repeat(5000)}));
        let bus_events = vec![small, large];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            model: &resolved,
            user_input: "test",
            blob_store: Some(&store),
            bus_events: &bus_events,
        };
        let section = provider.produce(&ctx).await.unwrap();
        // Small event inline
        assert!(section.content.contains("[topic: small]"));
        // Large event as blob reference
        assert!(section.content.contains("blob://test-source/big/"));
    }
}
