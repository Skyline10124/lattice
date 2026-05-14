use serde::{Deserialize, Serialize};

use crate::risk::RiskClass;

/// The specific kind of denial — carries typed context for each denial source.
///
/// Tagged enum uses `#[serde(tag = "type")]` so the JSON includes a `type` field
/// distinguishing each variant for model parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DenialKind {
    /// A shell command was denied by the command allowlist.
    Command {
        /// The full command that was denied (post-metacharacter check).
        command: String,
        /// The allowed command prefixes from the static allowlist.
        allowed_prefixes: Vec<String>,
    },
    /// A tool call was denied because the tool is not enabled.
    Tool {
        /// The name of the tool that was denied.
        tool_name: String,
        /// The list of tools that are currently enabled for this agent.
        enabled_tools: Vec<String>,
    },
    /// A filesystem path access was denied by the sandbox allowlist.
    Path {
        /// The path that was denied.
        path: String,
        /// The operation attempted (e.g., "read", "write").
        operation: String,
        /// The current path allowlist entries.
        allowlist: Vec<String>,
    },
}

/// A structured denial report returned when a sandbox check or tool
/// authorization fails.
///
/// Replaces opaque `String` denials (per D-08, D-09). Includes a
/// `__denial_report__` sentinel field (Pitfall 3 in RESEARCH.md) so the
/// agent loop can reliably distinguish denial results from normal tool
/// output that happens to contain JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenialReport {
    /// Sentinel field for reliable detection of denial JSON in tool results.
    /// Always serializes as `"__denial_report__": true`.
    #[serde(rename = "__denial_report__")]
    pub sentinel: bool,

    /// Human-readable reason explaining what was denied and why.
    pub reason_text: String,

    /// Risk classification of the denied operation.
    pub risk_class: RiskClass,

    /// Typed details about the specific denial.
    pub kind: DenialKind,

    /// An optional rule suggestion that would have allowed this operation.
    /// Filled by the agent loop, not the sandbox layer (per D-10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_rule: Option<String>,
}

impl DenialReport {
    /// Create a new `DenialReport` with the sentinel set to `true` and
    /// no suggested rule (filled later by the agent loop).
    pub fn new(reason_text: String, risk_class: RiskClass, kind: DenialKind) -> Self {
        Self {
            sentinel: true,
            reason_text,
            risk_class,
            kind,
            suggested_rule: None,
        }
    }

    /// Serialize this report to a JSON string.
    ///
    /// The sentinel field `__denial_report__` will be `true` in the output.
    /// If serialization fails (should never happen), returns a JSON error literal.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_denial_report_serialization_roundtrip() {
        let report = DenialReport::new(
            "command 'cargo publish' does not match any allowlist entry".to_string(),
            RiskClass::Mutate,
            DenialKind::Command {
                command: "cargo publish".to_string(),
                allowed_prefixes: vec![
                    "cargo test".to_string(),
                    "cargo build".to_string(),
                    "cargo check".to_string(),
                ],
            },
        );

        let json = serde_json::to_string(&report).expect("serialize");
        assert!(
            json.contains("\"__denial_report__\":true"),
            "JSON should contain sentinel: {}",
            json
        );

        let deserialized: DenialReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized, report, "roundtrip should preserve equality");
    }

    #[test]
    fn test_denial_report_sentinel_present() {
        let report = DenialReport::new(
            "test denial".to_string(),
            RiskClass::Read,
            DenialKind::Path {
                path: "/etc/shadow".to_string(),
                operation: "read".to_string(),
                allowlist: vec!["/home/user".to_string()],
            },
        );
        let json = report.to_json();
        assert!(
            json.contains("\"__denial_report__\":true"),
            "to_json should include sentinel: {}",
            json
        );
    }

    #[test]
    fn test_denial_kind_tagged() {
        let kind = DenialKind::Command {
            command: "rm -rf /".to_string(),
            allowed_prefixes: vec!["cargo test".to_string()],
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains("\"type\":\"command\""),
            "Command variant should serialize with type tag: {}",
            json
        );

        let tool_kind = DenialKind::Tool {
            tool_name: "shell".to_string(),
            enabled_tools: vec!["read".to_string(), "grep".to_string()],
        };
        let tool_json = serde_json::to_string(&tool_kind).unwrap();
        assert!(
            tool_json.contains("\"type\":\"tool\""),
            "Tool variant should serialize with type tag: {}",
            tool_json
        );

        let path_kind = DenialKind::Path {
            path: "/etc/passwd".to_string(),
            operation: "read".to_string(),
            allowlist: vec!["/home/user".to_string()],
        };
        let path_json = serde_json::to_string(&path_kind).unwrap();
        assert!(
            path_json.contains("\"type\":\"path\""),
            "Path variant should serialize with type tag: {}",
            path_json
        );
    }
}
