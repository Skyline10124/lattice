use serde::{Deserialize, Serialize};

/// Risk classification for denied operations.
///
/// Used by the approval UI to show risk context and by the model to
/// decide whether to suggest approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// Reading data (lowest risk: no modification)
    Read,
    /// Modifying data (medium risk: changes that can be undone with version control)
    Mutate,
    /// Irreversible/destructive operations (highest risk: deletion, system changes)
    Destructive,
}

/// Filesystem operation classification for path approval rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathOperation {
    Read,
    Write,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_risk_class_serialization() {
        assert_eq!(serde_json::to_string(&RiskClass::Read).unwrap(), "\"read\"");
        assert_eq!(
            serde_json::to_string(&RiskClass::Mutate).unwrap(),
            "\"mutate\""
        );
        assert_eq!(
            serde_json::to_string(&RiskClass::Destructive).unwrap(),
            "\"destructive\""
        );
    }

    #[test]
    fn test_path_operation_serialization() {
        assert_eq!(
            serde_json::to_string(&PathOperation::Read).unwrap(),
            "\"read\""
        );
        assert_eq!(
            serde_json::to_string(&PathOperation::Write).unwrap(),
            "\"write\""
        );
    }
}
