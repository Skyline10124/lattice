//! Session state mutation methods.

use crate::method::{CatalogMethod, CepMethod};
use lancet_protocol::RequiredCapability;
use lancet_protocol::{EffectDecl, EffectKindSet, EffectScopeKind, ImpactKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `session.todo_write`
pub struct TodoWrite;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TodoWriteParams {
    pub todos: Vec<TodoItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TodoWriteResult {
    pub todos: Vec<TodoItem>,
}

impl CepMethod for TodoWrite {
    const NAME: &'static str = "session.todo_write";
    const DESCRIPTION: &'static str =
        "Record the current task plan as a concise todo list for multi-step work.";
    type Params = TodoWriteParams;
    type Result = TodoWriteResult;
    const IMPACT: ImpactKind = ImpactKind::SessionStateMutation;
    const CAPABILITY: RequiredCapability = RequiredCapability::MutateSessionState;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::SessionStateMutation]),
        scope: EffectScopeKind::SessionState,
        capabilities: &[RequiredCapability::MutateSessionState],
    };
}

impl CatalogMethod for TodoWrite {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_todo_write_name_is_dotted() {
        assert_eq!(TodoWrite::NAME, "session.todo_write");
        assert!(TodoWrite::NAME.contains('.'));
        assert!(!TodoWrite::NAME.contains('/'));
    }
}
