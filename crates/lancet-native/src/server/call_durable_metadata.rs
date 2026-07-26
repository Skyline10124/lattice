// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_boundary_metadata_survives_later_merge_update() {
        let metadata = CallDurableMetadata::default();
        metadata.record(json!({
            "command_isolation_mode": "enforce",
            "effective_boundary": {
                "network": {
                    "boundary": "network",
                    "status": "failed_closed",
                    "diagnostics": {
                        "boundary_setup_failed_closed": "true"
                    }
                }
            },
            "host_probe_summary": {
                "primitives": {}
            },
            "boundary_setup_failed_closed": true,
            "install_completed": false,
            "policy_enforced": true
        }));
        metadata.record(json!({
            "install_completed": true,
            "exit_code": 127,
            "policy_enforced": true
        }));

        let merged: Value =
            serde_json::from_str(&metadata.take_json().expect("metadata json")).unwrap();
        assert_eq!(merged["install_completed"], true);
        assert_eq!(
            merged["effective_boundary"]["network"]["status"],
            "failed_closed"
        );
        assert_eq!(merged["boundary_setup_failed_closed"], true);
        assert_eq!(merged["exit_code"], 127);
    }
}
