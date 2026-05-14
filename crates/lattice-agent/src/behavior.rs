pub use lattice_core::behavior::{BehaviorMode, YoloSandboxPolicy};

use crate::Agent;

/// Guard that restores `BehaviorMode` on drop, ensuring panic safety.
struct BehaviorGuard {
    target: *mut BehaviorMode,
    saved: BehaviorMode,
    audit: Option<std::sync::Arc<crate::audit::AuditLog>>,
}

impl BehaviorGuard {
    /// Disarm the guard and return the saved behavior without restoring on drop.
    fn disarm(mut self) -> BehaviorMode {
        let saved = std::mem::take(&mut self.saved);
        // Mark as disarmed by setting target to null
        self.target = std::ptr::null_mut();
        std::mem::forget(self);
        saved
    }
}

impl Drop for BehaviorGuard {
    fn drop(&mut self) {
        if !self.target.is_null() {
            if let Some(ref audit) = self.audit {
                audit.log_sync(crate::audit::AuditEvent::behavior_switch(
                    "Yolo(panic_unwind)",
                    &format!("{:?}", self.saved),
                    "permissive_scope_panic_recovery",
                ));
            }
            // SAFETY: target is valid for the lifetime of Agent::with_permissive
            unsafe { *self.target = self.saved.clone() };
        }
    }
}

// SAFETY: BehaviorGuard is only used within Agent::with_permissive where the
// pointer lifetime is guaranteed by the &mut self borrow.
unsafe impl Send for BehaviorGuard {}

impl Agent {
    /// Switch the agent's runtime behavior mode.
    ///
    /// `Strict` mode (the default) enforces output validation and confidence
    /// thresholds. `Yolo` mode allows tool execution without validation,
    /// constrained by the given sandbox policy.
    pub fn set_behavior(&mut self, mode: BehaviorMode) {
        let from = format!("{:?}", self.behavior);
        let to = format!("{:?}", mode);
        tracing::info!("Agent behavior switched from {} to {}", from, to);
        if let Some(ref audit) = self.audit {
            audit.log_sync(crate::audit::AuditEvent::behavior_switch(
                &from,
                &to,
                "user_request",
            ));
        }
        self.behavior = mode;
    }

    /// Return the current behavior mode.
    pub fn behavior(&self) -> &BehaviorMode {
        &self.behavior
    }

    /// Temporarily run with permissive/YOLO mode with command-allowlist sandbox.
    ///
    /// Uses a Drop guard to guarantee behavior restoration even on panic.
    pub async fn with_permissive<Fut, T>(
        &mut self,
        f: impl FnOnce() -> Fut,
    ) -> Result<T, lattice_core::LatticeError>
    where
        Fut: std::future::Future<Output = Result<T, lattice_core::LatticeError>> + Send,
    {
        let saved = self.behavior.clone();
        let yolo_mode = BehaviorMode::Yolo {
            enforce_sandbox: YoloSandboxPolicy::EnforceCommandAllowlist,
        };

        if let Some(ref audit) = self.audit {
            audit.log_sync(crate::audit::AuditEvent::behavior_switch(
                &format!("{:?}", saved),
                &format!("{:?}", yolo_mode),
                "permissive_scope",
            ));
        }
        self.behavior = yolo_mode;

        // SAFETY: BehaviorGuard restores `self.behavior` on drop, including
        // unwind paths. We use a raw pointer because the guard and the async
        // closure both need `&mut self` — the guard only writes on drop after
        // the closure has finished or panicked.
        let guard = BehaviorGuard {
            target: &mut self.behavior as *mut BehaviorMode,
            saved: saved.clone(),
            audit: self.audit.clone(),
        };

        let result = f().await;

        // Normal path: disarm the guard and restore explicitly for audit.
        let guard_saved = guard.disarm();
        if let Some(ref audit) = self.audit {
            audit.log_sync(crate::audit::AuditEvent::behavior_switch(
                &format!("{:?}", self.behavior),
                &format!("{:?}", guard_saved),
                "permissive_scope",
            ));
        }
        self.behavior = guard_saved;

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::catalog::ApiProtocol;
    use lattice_core::ResolvedModel;
    use std::collections::HashMap;

    fn make_resolved(context_length: u32) -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test".into(),
            api_model_id: "test".into(),
            provider: "test".into(),
            base_url: "http://localhost".to_string(),
            api_key: Some("sk-test".into()),
            api_protocol: ApiProtocol::OpenAiChat,
            context_length,
            credential_status: lattice_core::CredentialStatus::Present,
            provider_specific: HashMap::new(),
        }
    }

    #[test]
    fn test_default_behavior_is_strict() {
        let b = BehaviorMode::default();
        assert!(b.is_strict());
        assert!(!b.is_yolo());
    }

    #[test]
    fn test_yolo_behavior() {
        let b = BehaviorMode::Yolo {
            enforce_sandbox: YoloSandboxPolicy::EnforceCommandAllowlist,
        };
        assert!(b.is_yolo());
        assert!(!b.is_strict());
    }

    #[test]
    fn test_behavior_mode_clone() {
        let b = BehaviorMode::Strict {
            confidence_threshold: 0.9,
            max_retries: 5,
            escalate_to: Some("lead".into()),
        };
        let b2 = b.clone();
        assert_eq!(b, b2);
    }

    mod agent_behavior {
        use super::*;

        #[test]
        fn test_default_behavior_is_strict() {
            let resolved = make_resolved(16384);
            let agent = Agent::new(resolved);
            assert!(agent.behavior().is_strict());
            assert!(!agent.behavior().is_yolo());
        }

        #[test]
        fn test_set_behavior_switches_mode() {
            let resolved = make_resolved(16384);
            let mut agent = Agent::new(resolved);

            // Switch to Yolo
            agent.set_behavior(BehaviorMode::Yolo {
                enforce_sandbox: YoloSandboxPolicy::EnforceCommandAllowlist,
            });
            assert!(agent.behavior().is_yolo());
            assert!(!agent.behavior().is_strict());

            // Switch back to Strict
            agent.set_behavior(BehaviorMode::Strict {
                confidence_threshold: 0.9,
                max_retries: 5,
                escalate_to: None,
            });
            assert!(agent.behavior().is_strict());
            assert!(!agent.behavior().is_yolo());
        }

        #[test]
        fn test_behavior_getter_returns_ref() {
            let resolved = make_resolved(16384);
            let mut agent = Agent::new(resolved);
            assert!(agent.behavior().is_strict());

            agent.set_behavior(BehaviorMode::Yolo {
                enforce_sandbox: YoloSandboxPolicy::NoBash,
            });
            assert_eq!(
                agent.behavior(),
                &BehaviorMode::Yolo {
                    enforce_sandbox: YoloSandboxPolicy::NoBash,
                }
            );
        }

        #[tokio::test]
        async fn test_with_permissive_switches_and_restores_on_ok() {
            let resolved = make_resolved(16384);
            let mut agent = Agent::new(resolved);
            let original = agent.behavior().clone();

            let result = agent
                .with_permissive(|| async {
                    // Inside the closure, behavior should be Yolo
                    Ok::<_, lattice_core::LatticeError>(42)
                })
                .await;

            assert_eq!(result.unwrap(), 42);
            // Behavior should be restored to original (Strict default)
            assert_eq!(agent.behavior(), &original);
        }

        #[tokio::test]
        async fn test_with_permissive_restores_on_err() {
            let resolved = make_resolved(16384);
            let mut agent = Agent::new(resolved);
            let original = agent.behavior().clone();

            let result: Result<(), _> = agent
                .with_permissive(|| async {
                    Err(lattice_core::LatticeError::Config {
                        message: "test error".into(),
                    })
                })
                .await;

            assert!(result.is_err());
            // Behavior should still be restored
            assert_eq!(agent.behavior(), &original);
        }

        #[tokio::test]
        async fn test_with_permissive_preserves_custom_strict() {
            let resolved = make_resolved(16384);
            let mut agent = Agent::new(resolved);
            let custom_strict = BehaviorMode::Strict {
                confidence_threshold: 0.95,
                max_retries: 7,
                escalate_to: Some("lead".into()),
            };
            agent.set_behavior(custom_strict.clone());

            let result = agent
                .with_permissive(|| async { Ok::<_, lattice_core::LatticeError>("done") })
                .await;

            assert_eq!(result.unwrap(), "done");
            // Should restore the custom Strict, not the default
            assert_eq!(agent.behavior(), &custom_strict);
        }
    }
}
