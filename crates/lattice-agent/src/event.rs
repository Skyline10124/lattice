use crate::TurnManifestObject;

#[derive(Debug, Clone)]
pub enum LoopEvent {
    Token {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCallRequired {
        calls: Vec<lattice_core::types::ToolCall>,
    },
    ToolResult {
        call: lattice_core::types::ToolCall,
        result: String,
    },
    Manifest {
        manifest: TurnManifestObject,
    },
    Done {
        usage: Option<lattice_core::streaming::TokenUsage>,
    },
    Error {
        message: String,
        retryable: bool,
    },
}
