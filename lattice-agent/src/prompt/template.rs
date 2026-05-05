use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("template variable '{0}' is not allowed")]
    UnknownVariable(String),
    #[error("template variable '{0}' is missing")]
    MissingVariable(String),
    #[error("template has an unclosed variable")]
    UnclosedVariable,
    #[error("template variable is empty")]
    EmptyVariable,
}

/// Minimal trusted-variable renderer for system prompt templates.
///
/// Supports only `{{name}}`. No conditionals, loops, filters, or function calls.
pub fn render_template(
    template: &str,
    variables: &HashMap<String, String>,
    allowed: &[&str],
) -> Result<String, TemplateError> {
    let mut rendered = String::new();
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        rendered.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            return Err(TemplateError::UnclosedVariable);
        };
        let name = after_open[..end].trim();
        if name.is_empty() {
            return Err(TemplateError::EmptyVariable);
        }
        if !allowed.contains(&name) {
            return Err(TemplateError::UnknownVariable(name.to_string()));
        }
        let value = variables
            .get(name)
            .ok_or_else(|| TemplateError::MissingVariable(name.to_string()))?;
        rendered.push_str(value);
        rest = &after_open[end + 2..];
    }

    rendered.push_str(rest);
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> HashMap<String, String> {
        HashMap::from([
            ("role".to_string(), "reviewer".to_string()),
            ("mission".to_string(), "find bugs".to_string()),
        ])
    }

    #[test]
    fn renders_allowed_variables() {
        let rendered = render_template(
            "You are a {{ role }}. {{mission}}.",
            &vars(),
            &["role", "mission"],
        )
        .unwrap();
        assert_eq!(rendered, "You are a reviewer. find bugs.");
    }

    #[test]
    fn rejects_unknown_variables() {
        let err = render_template("{{user_input}}", &vars(), &["role"]).unwrap_err();
        assert_eq!(err, TemplateError::UnknownVariable("user_input".into()));
    }

    #[test]
    fn rejects_missing_variables() {
        let err = render_template("{{mission}}", &HashMap::new(), &["mission"]).unwrap_err();
        assert_eq!(err, TemplateError::MissingVariable("mission".into()));
    }
}
