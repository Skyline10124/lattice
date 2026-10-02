// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP frame aliases over JSON-RPC 2.0 types.
//!
//! This type alias gives CEP-specific semantics to a JSON-RPC notification.

/// A CEP tool event — a JSON-RPC notification (no id) for streaming progress.
pub type ToolEvent<P = serde_json::Value> = crate::jsonrpc::Notification<P>;

#[cfg(test)]
mod tests {
    use crate::error::{CepError, CepErrorCode, CepErrorData, ExecutionErrorData, Retriability};
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
        let tool_err: ErrorObject<CepError> = ErrorObject {
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
