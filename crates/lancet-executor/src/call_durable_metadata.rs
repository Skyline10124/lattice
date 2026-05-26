//! Per-call compact metadata accumulated by native handlers for durable audit rows.

use serde_json::Value;
use std::sync::{Arc, Mutex};

/// Turn-scoped metadata written by handlers and read by transport finalization.
#[derive(Clone, Debug, Default)]
pub(crate) struct CallDurableMetadata(Arc<Mutex<Option<Value>>>);

impl CallDurableMetadata {
    /// Record or merge compact handler metadata (last write wins on key collision).
    pub(crate) fn record(&self, metadata: Value) {
        let mut guard = self.0.lock().expect("durable metadata mutex poisoned");
        match guard.as_mut() {
            None => *guard = Some(metadata),
            Some(existing) => {
                if let (Some(existing_obj), Some(new_obj)) =
                    (existing.as_object_mut(), metadata.as_object())
                {
                    for (key, value) in new_obj {
                        existing_obj.insert(key.clone(), value.clone());
                    }
                } else {
                    *guard = Some(metadata);
                }
            }
        }
    }

    /// Take the accumulated metadata as a JSON string for `metadata_json` persistence.
    pub(crate) fn take_json(&self) -> Option<String> {
        self.0
            .lock()
            .expect("durable metadata mutex poisoned")
            .take()
            .map(|value| value.to_string())
    }
}
