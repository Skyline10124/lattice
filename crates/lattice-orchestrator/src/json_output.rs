/// Max retries for JSON schema validation failures.
pub const MAX_SCHEMA_RETRIES: u32 = 2;

pub fn strip_markdown_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    }
}

pub fn parse_json_or_content(content: &str) -> serde_json::Value {
    serde_json::from_str(strip_markdown_fence(content))
        .unwrap_or_else(|_| serde_json::json!({ "content": content }))
}

/// Check if agent output content looks like an error message rather than
/// a genuine attempt to produce structured output.
pub fn looks_like_error(content: &str) -> bool {
    let trimmed = content.trim();
    let lower = trimmed.to_ascii_lowercase();

    const CASE_SENSITIVE_PREFIXES: &[&str] = &[
        "Error:",
        "error:",
        "Provider error:",
        "API error:",
        "Authentication error:",
        "Rate limit",
        "401",
        "403",
        "429",
        "500",
        "502",
        "503",
        "504",
    ];
    const LOWER_PREFIXES: &[&str] = &[
        "rate limit",
        "authentication",
        "unauthorized",
        "forbidden",
        "api key",
        "invalid api key",
        "invalid request",
        "bad request",
        "server error",
        "internal server error",
        "service unavailable",
        "temporarily unavailable",
        "timeout",
        "request timeout",
        "connection refused",
        "connection reset",
        "connection closed",
        "network error",
        "dns",
        "tls",
        "certificate",
    ];
    const LOWER_CONTAINS: &[&str] = &[
        "\"error\"",
        "context_length_exceeded",
        "rate_limit",
        "invalid_api_key",
    ];

    CASE_SENSITIVE_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
        || LOWER_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        || LOWER_CONTAINS.iter().any(|needle| lower.contains(needle))
}

pub fn parse_assistant_output(
    content: &str,
    require_json: bool,
) -> Result<serde_json::Value, String> {
    if content.is_empty() && require_json {
        return Err(
            "Agent produced empty output; output_schema requires structured response".into(),
        );
    }
    if looks_like_error(content) {
        return Err(format!("Agent returned error-like output: {content}"));
    }

    match serde_json::from_str(strip_markdown_fence(content)) {
        Ok(value) => Ok(value),
        Err(_) if require_json => {
            Err("Agent output is not valid JSON; output_schema requires structured output".into())
        }
        Err(_) => Ok(serde_json::json!({"_raw_text": true, "content": content})),
    }
}

pub struct CompiledOutputSchema {
    schema_json: serde_json::Value,
    validator: jsonschema::Validator,
}

impl CompiledOutputSchema {
    pub fn compile(agent_name: &str, schema: &str) -> Result<Self, String> {
        let schema_json = serde_json::from_str::<serde_json::Value>(schema)
            .map_err(|err| format!("output_schema is not valid JSON for {agent_name}: {err}"))?;
        let validator = jsonschema::validator_for(&schema_json).map_err(|err| {
            format!("output_schema is not a valid JSON Schema for {agent_name}: {err}")
        })?;
        Ok(Self {
            schema_json,
            validator,
        })
    }

    pub fn validation_errors(&self, output: &serde_json::Value) -> Vec<String> {
        self.validator
            .iter_errors(output)
            .take(3)
            .map(|err| format!("- {err}"))
            .collect()
    }

    pub fn correction_hint(&self, input: &str, errors: &[String]) -> String {
        format!(
            "{}\n\nYour previous response did not match the required JSON format. \
             Errors:\n{}\n\nExpected schema:\n{}\n\nPlease correct your response. \
             Return ONLY valid JSON that matches the schema, no markdown.",
            input,
            errors.join("\n"),
            serde_json::to_string_pretty(&self.schema_json).unwrap_or_default()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_assistant_output_requires_json_when_schema_bound() {
        let err = parse_assistant_output("plain text", true).unwrap_err();
        assert!(err.contains("output_schema requires structured output"));
    }

    #[test]
    fn parse_assistant_output_wraps_raw_text_for_plain_profiles() {
        let value = parse_assistant_output("plain text", false).unwrap();
        assert_eq!(value["_raw_text"], true);
        assert_eq!(value["content"], "plain text");
    }

    #[test]
    fn compiled_schema_reports_validation_errors() {
        let schema = CompiledOutputSchema::compile(
            "agent",
            r#"{
                "type": "object",
                "required": ["answer"],
                "properties": { "answer": { "type": "string" } }
            }"#,
        )
        .unwrap();

        let errors = schema.validation_errors(&serde_json::json!({"other": true}));

        assert!(!errors.is_empty());
    }
}
