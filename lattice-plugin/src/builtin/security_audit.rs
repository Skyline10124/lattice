use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditInput {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub threat_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vulnerability {
    pub severity: String,
    pub category: String,
    pub location: String,
    pub description: String,
    pub remediation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditOutput {
    #[serde(default)]
    pub vulnerabilities: Vec<Vulnerability>,
    #[serde(default)]
    pub risk_score: f64,
}

pub struct SecurityAuditPlugin;

impl SecurityAuditPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for SecurityAuditPlugin {
    type Input = SecurityAuditInput;
    type Output = SecurityAuditOutput;

    fn name(&self) -> &str {
        "security-audit"
    }

    fn system_prompt(&self) -> &str {
        "You are a security auditor. Review code for OWASP Top 10 vulnerabilities. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Audit for security issues.\nCODE:\n{}\nDEPENDENCIES:\n{}\nTHREAT MODEL:\n{}",
            input.code,
            input.dependencies.join("\n"),
            input.threat_model
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
