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
    use crate::frame::{ToolCall, ToolError, ToolEvent, ToolResult};
    use crate::jsonrpc::{ErrorObject, Id};
    use serde_json::json;

    #[test]
    fn tool_call_is_jsonrpc_request() {
        let call: ToolCall = crate::jsonrpc::Request::new(
            Id::parse("11111111-1111-4111-8111-111111111111").expect("valid InvocationId"),
            "read_file".into(),
            json!({"path": "/etc/hosts"}),
        );
        let serialized = serde_json::to_string(&call).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["method"], "read_file");
        assert_eq!(parsed["params"]["path"], "/etc/hosts");
    }

    #[test]
    fn tool_event_is_jsonrpc_notification() {
        let event: ToolEvent =
            crate::jsonrpc::Notification::new("progress".into(), json!({"percent": 75}));
        let serialized = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert!(parsed.get("id").is_none());
        assert_eq!(parsed["method"], "progress");
    }

    #[test]
    fn tool_result_success() {
        let result: ToolResult = crate::jsonrpc::Response::Success {
            result: json!({"content": "hello"}),
            id: Id::parse("11111111-1111-4111-8111-111111111111").expect("valid InvocationId"),
        };
        let serialized = serde_json::to_string(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["result"]["content"], "hello");
    }

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
