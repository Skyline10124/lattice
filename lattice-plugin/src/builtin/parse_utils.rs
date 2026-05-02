/// Extract a confidence score from LLM response text.
pub fn extract_confidence(raw: &str) -> f64 {
    for line in raw.lines() {
        if let Some((_, after)) = line.split_once("\"confidence\"") {
            if let Some(colon) = after.find(':') {
                let val = after[colon + 1..]
                    .trim()
                    .trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
                if let Ok(f) = val.parse::<f64>() {
                    return f.clamp(0.0, 1.0);
                }
            }
        }
    }
    0.0
}

/// Strip markdown code fences from LLM response.
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

/// Parse JSON from LLM response, stripping markdown fences.
pub fn parse_json_from_response(raw: &str) -> Result<serde_json::Value, serde_json::Error> {
    let cleaned = strip_markdown_fence(raw);
    serde_json::from_str(cleaned)
}
