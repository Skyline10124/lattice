use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::risk::{PathOperation, RiskClass};

/// The specific type of an approval rule — what is being allowed.
///
/// Explicit vocabulary distinguishes exact matches from prefix/recursive:
/// - `CommandExact` / `PathExact` — only the literal command or path
/// - `CommandPrefix` / `PathRecursive` — allow anything under the prefix/root
///
/// Tagged enum uses `#[serde(tag = "type")]` so the JSON includes a `type` field
/// distinguishing each variant for model and UI parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalKind {
    /// Allow an exact shell command (no subcommands or additional args).
    CommandExact {
        /// The full command to allow, exactly as written.
        command: String,
    },
    /// Allow commands starting with this prefix (e.g., "cargo test").
    CommandPrefix {
        /// The command prefix to allow.
        prefix: String,
    },
    /// Allow filesystem access to exactly this path with the specified operation.
    PathExact {
        /// The exact path being allowed.
        path: PathBuf,
        /// The operation allowed (read or write).
        operation: PathOperation,
    },
    /// Allow filesystem access to any path under this root with the specified
    /// operation (recursive, component-boundary prefix matching).
    PathRecursive {
        /// The root directory under which access is allowed.
        root: PathBuf,
        /// The operation allowed (read or write).
        operation: PathOperation,
    },
    /// Allow a specific named tool.
    Tool {
        /// The name of the tool being allowed.
        tool_name: String,
    },
    /// Allow an entire tool bundle.
    Bundle {
        /// The name of the tool bundle (must match a ToolBundle variant).
        bundle_name: String,
    },
}

/// A validated approval grant — an allowlisted operation with an associated risk class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalGrant {
    /// The specific operation being allowed.
    pub kind: ApprovalKind,
    /// The risk classification that was accepted for this rule.
    pub risk_class: RiskClass,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_approval_kind_command_prefix_serialization() {
        let kind = ApprovalKind::CommandPrefix {
            prefix: "cargo test".to_string(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains("\"type\":\"command_prefix\""),
            "JSON should contain type tag: {}",
            json
        );
        let deserialized: ApprovalKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, kind);
    }

    #[test]
    fn test_approval_kind_command_exact_serialization() {
        let kind = ApprovalKind::CommandExact {
            command: "cargo build --release".to_string(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains("\"type\":\"command_exact\""),
            "JSON should contain type tag: {}",
            json
        );
        let deserialized: ApprovalKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, kind);
    }

    #[test]
    fn test_approval_kind_path_recursive_serialization() {
        let kind = ApprovalKind::PathRecursive {
            root: PathBuf::from("/home/user/project"),
            operation: PathOperation::Read,
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains("\"type\":\"path_recursive\""),
            "JSON should contain type tag: {}",
            json
        );
        let deserialized: ApprovalKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, kind);
    }

    #[test]
    fn test_approval_kind_path_exact_serialization() {
        let kind = ApprovalKind::PathExact {
            path: PathBuf::from("/home/user/config.toml"),
            operation: PathOperation::Write,
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains("\"type\":\"path_exact\""),
            "JSON should contain type tag: {}",
            json
        );
        let deserialized: ApprovalKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, kind);
    }

    #[test]
    fn test_approval_kind_tool_serialization() {
        let kind = ApprovalKind::Tool {
            tool_name: "read_file".to_string(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains("\"type\":\"tool\""),
            "JSON should contain type tag: {}",
            json
        );
        let deserialized: ApprovalKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, kind);
    }

    #[test]
    fn test_approval_kind_bundle_serialization() {
        let kind = ApprovalKind::Bundle {
            bundle_name: "workspace".to_string(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains("\"type\":\"bundle\""),
            "JSON should contain type tag: {}",
            json
        );
        let deserialized: ApprovalKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, kind);
    }

    #[test]
    fn test_approval_grant_serialization() {
        let grant = ApprovalGrant {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo test".to_string(),
            },
            risk_class: RiskClass::Mutate,
        };

        let json = serde_json::to_string(&grant).expect("serialize");
        assert!(
            json.contains("\"type\":\"command_prefix\""),
            "JSON should contain type tag: {}",
            json
        );

        let deserialized: ApprovalGrant = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized, grant, "roundtrip should preserve equality");
    }
}
