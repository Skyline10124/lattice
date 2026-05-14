use serde::{Deserialize, Serialize};

use crate::denial::DenialReport;

/// The outcome of a policy check — either allow or deny with a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyDecision {
    /// The operation is allowed.
    Allow,
    /// The operation is denied; the enclosed report explains why.
    Deny(DenialReport),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::denial::DenialKind;
    use crate::risk::RiskClass;

    #[test]
    fn test_policy_decision_allow_serialization() {
        let decision = PolicyDecision::Allow;
        let json = serde_json::to_string(&decision).unwrap();
        assert_eq!(json, "\"Allow\"");
        let deserialized: PolicyDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, decision);
    }

    #[test]
    fn test_policy_decision_deny_serialization() {
        let report = DenialReport::new(
            "denied".to_string(),
            RiskClass::Read,
            DenialKind::Path {
                path: "/etc/shadow".to_string(),
                operation: "read".to_string(),
                allowlist: vec![],
            },
        );
        let decision = PolicyDecision::Deny(report.clone());
        let json = serde_json::to_string(&decision).unwrap();
        assert!(
            json.contains("\"Deny\""),
            "PolicyDecision::Deny should serialize with variant name: {}",
            json
        );
        let deserialized: PolicyDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, decision);
    }
}
