use crate::types::ToolCall;

pub struct ToolCallRequest {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: String,
}

pub struct ToolCallResult {
    pub tool_call_id: String,
    pub output: String,
    pub is_error: bool,
}

impl ToolCallRequest {
    pub fn from_tool_call(tc: &ToolCall) -> Self {
        ToolCallRequest {
            tool_call_id: tc.id.clone(),
            function_name: tc.function.name.clone(),
            arguments: tc.function.arguments.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FunctionCall;

    #[test]
    fn test_tool_call_request_from_tool_call() {
        let tc = ToolCall {
            id: "call_1".into(),
            function: FunctionCall {
                name: "test_fn".into(),
                arguments: "{}".into(),
            },
        };
        let req = ToolCallRequest::from_tool_call(&tc);
        assert_eq!(req.tool_call_id, "call_1");
        assert_eq!(req.function_name, "test_fn");
        assert_eq!(req.arguments, "{}");
    }

    #[test]
    fn test_tool_call_request_preserves_complex_arguments() {
        let tc = ToolCall {
            id: "call_2".into(),
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city": "Tokyo", "units": "metric"}"#.into(),
            },
        };
        let req = ToolCallRequest::from_tool_call(&tc);
        assert_eq!(req.tool_call_id, "call_2");
        assert_eq!(req.function_name, "get_weather");
        assert_eq!(req.arguments, r#"{"city": "Tokyo", "units": "metric"}"#);
    }

    #[test]
    fn test_tool_call_result_construction() {
        let result = ToolCallResult {
            tool_call_id: "call_1".into(),
            output: "result data".into(),
            is_error: false,
        };
        assert_eq!(result.tool_call_id, "call_1");
        assert_eq!(result.output, "result data");
        assert!(!result.is_error);
    }

    #[test]
    fn test_tool_call_result_error_case() {
        let result = ToolCallResult {
            tool_call_id: "call_err".into(),
            output: "something went wrong".into(),
            is_error: true,
        };
        assert!(result.is_error);
        assert_eq!(result.tool_call_id, "call_err");
    }
}
