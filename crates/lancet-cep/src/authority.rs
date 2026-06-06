// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP base authority planning.

use std::error::Error;
use std::fmt;

use lancet_catalog::BaseAuthorityRequirement;
use lancet_protocol::BaseExecutionEnvelope;

const DEFAULT_INTENT_DESCRIPTION: &str = "cep_context turn observation";

/// Explicit plan for turn base authority.
///
/// This is intentionally separate from tool exposure: exposure decides which
/// methods are visible to a caller, while this plan decides which base
/// capabilities are present before any active execution envelope. The built
/// envelope may include session/fetch/audit authority, but never workspace write
/// or command authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseAuthorityPlan {
    project_root: String,
    intent_description: String,
    session_state: bool,
    fetch_hosts: Vec<String>,
    read_audit: bool,
}

impl BaseAuthorityPlan {
    /// Start a base authority plan rooted at `project_root`.
    pub fn for_workspace_root(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
            intent_description: DEFAULT_INTENT_DESCRIPTION.to_string(),
            session_state: false,
            fetch_hosts: Vec::new(),
            read_audit: false,
        }
    }

    /// Override the intent description recorded in the base envelope.
    pub fn with_intent_description(mut self, description: impl Into<String>) -> Self {
        self.intent_description = description.into();
        self
    }

    /// Grant session-state mutation in the base envelope.
    pub fn with_session_state(mut self) -> Self {
        self.session_state = true;
        self
    }

    /// Grant fetch authority for one host.
    pub fn with_fetch_host(mut self, host: impl AsRef<str>) -> Self {
        let host = host.as_ref().trim();
        if !host.is_empty() && !self.fetch_hosts.iter().any(|known| known == host) {
            self.fetch_hosts.push(host.to_string());
        }
        self
    }

    /// Grant fetch authority for multiple hosts.
    pub fn with_fetch_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for host in hosts {
            self = self.with_fetch_host(host);
        }
        self
    }

    /// Grant read-audit authority in the base envelope.
    pub fn with_read_audit(mut self) -> Self {
        self.read_audit = true;
        self
    }

    /// Apply catalog-declared base authority requirements.
    pub fn with_requirements<I, S>(
        mut self,
        requirements: BaseAuthorityRequirement,
        fetch_hosts: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if requirements.session_state {
            self = self.with_session_state();
        }
        if requirements.fetch_hosts {
            self = self.with_fetch_hosts(fetch_hosts);
        }
        if requirements.read_audit {
            self = self.with_read_audit();
        }
        self
    }

    /// Build the protocol base envelope and validate that it contains no
    /// workspace write or command authority.
    pub fn build(&self) -> Result<BaseExecutionEnvelope, BaseAuthorityError> {
        let mut base =
            BaseExecutionEnvelope::for_workspace_root(&self.project_root, &self.intent_description);
        if self.session_state {
            base = base.with_session_state();
        }
        for host in &self.fetch_hosts {
            base = base.with_fetch_host(host);
        }
        if self.read_audit {
            base = base.with_read_audit();
        }
        base.validate_no_write_or_command()
            .map_err(|message| BaseAuthorityError {
                message: message.to_string(),
            })?;
        Ok(base)
    }
}

/// Base authority plans can only build read/session/fetch/audit base envelopes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseAuthorityError {
    message: String,
}

impl BaseAuthorityError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BaseAuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for BaseAuthorityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::Capability;

    #[test]
    fn base_authority_plan_builds_read_audit_without_write_or_command() {
        let envelope = BaseAuthorityPlan::for_workspace_root("/workspace")
            .with_read_audit()
            .build()
            .unwrap();

        assert!(envelope.inner.capabilities.contains(&Capability::ReadPath {
            path_pattern: "/workspace".to_string()
        }));
        assert!(envelope.inner.capabilities.contains(&Capability::Inspect));
        assert!(envelope.inner.capabilities.contains(&Capability::ReadAudit));
        assert!(envelope.validate_no_write_or_command().is_ok());
    }

    #[test]
    fn base_authority_plan_grants_session_and_fetch_explicitly() {
        let envelope = BaseAuthorityPlan::for_workspace_root("/workspace")
            .with_session_state()
            .with_fetch_hosts(["example.com", " example.com ", "", "api.github.com"])
            .build()
            .unwrap();

        assert!(envelope
            .inner
            .capabilities
            .contains(&Capability::MutateSessionState));
        assert!(envelope.inner.capabilities.contains(&Capability::FetchUrl {
            host: "example.com".to_string()
        }));
        assert!(envelope.inner.capabilities.contains(&Capability::FetchUrl {
            host: "api.github.com".to_string()
        }));
        assert_eq!(
            envelope
                .inner
                .capabilities
                .iter()
                .filter(|cap| matches!(cap, Capability::FetchUrl { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn base_authority_plan_applies_catalog_requirements() {
        let envelope = BaseAuthorityPlan::for_workspace_root("/workspace")
            .with_requirements(
                BaseAuthorityRequirement::new(true, true, true),
                ["example.com"],
            )
            .build()
            .unwrap();

        assert!(envelope
            .inner
            .capabilities
            .contains(&Capability::MutateSessionState));
        assert!(envelope.inner.capabilities.contains(&Capability::FetchUrl {
            host: "example.com".to_string()
        }));
        assert!(envelope.inner.capabilities.contains(&Capability::ReadAudit));
    }
}
