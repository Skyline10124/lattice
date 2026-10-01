// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP frame aliases over JSON-RPC 2.0 types.
//!
//! These type aliases give CEP-specific semantics to the domain-neutral
//! JSON-RPC types: `ToolCall` = request, `ToolResult` = response,
//! `ToolEvent` = notification, `ToolError` = error object.

/// A CEP tool call — a JSON-RPC request where `params` is the tool input.
pub type ToolCall<P = serde_json::Value> = crate::jsonrpc::Request<P>;

/// A CEP tool result — a JSON-RPC response with CEP-typed error data.
pub type ToolResult<R = serde_json::Value> = crate::jsonrpc::Response<R, crate::error::CepError>;

/// A CEP tool event — a JSON-RPC notification (no id) for streaming progress.
pub type ToolEvent<P = serde_json::Value> = crate::jsonrpc::Notification<P>;

/// A CEP tool error — a JSON-RPC error object with CEP-typed data.
pub type ToolError = crate::jsonrpc::ErrorObject<crate::error::CepError>;

#[cfg(test)]
mod tests {
    use crate::error::{CepError, CepErrorCode, CepErrorData, ExecutionErrorData, Retriability};
    use crate::frame::ToolError;
    use crate::jsonrpc::ErrorObject;

    #[test]
    fn tool_error_is_error_object_with_cep_data() {
        let cep_err = CepError {
            code: CepErrorCode::ExecutionFailed,
            message: "runtime operation failed".into(),
            data: CepErrorData::Execution(ExecutionErrorData {
                operation: "mutate.edit".into(),
                detail: Some("installation failed".into()),
            }),
            retriability: Retriability::Transient,
        };
        let tool_err: ToolError = ErrorObject {
            code: cep_err.code.as_i32(),
            message: cep_err.message.clone(),
            data: Some(cep_err),
        };
        let serialized = serde_json::to_string(&tool_err).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["code"], -32020);
        assert_eq!(parsed["data"]["retriability"], "transient");
    }
}
