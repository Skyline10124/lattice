// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Agent-visible CEP tool exposure.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use lancet_catalog::{BaseAuthorityRequirement, MethodDescriptor, catalog};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Domain separator for native MethodDescriptor identity digests.
const NATIVE_DESCRIPTOR_DOMAIN: &[u8] = b"lancet-native-descriptor-identity-v1\0";

/// A CEP method projected as an LLM-facing tool definition.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON schema for the tool parameters.
    pub parameters: serde_json::Value,
    /// Stable descriptor identity (lowercase SHA-256 hex).
    ///
    /// Native methods digest the canonical MethodDescriptor bytes; Component
    /// Tools carry the host-pinned `descriptor_sha256` of the embedded
    /// descriptor JSON. Never a placeholder.
    pub descriptor_identity: String,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        descriptor_identity: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            descriptor_identity: descriptor_identity.into(),
        }
    }
}

/// Digest hex of the canonical native MethodDescriptor bytes.
pub fn native_descriptor_identity(descriptor: &MethodDescriptor) -> String {
    let params = (descriptor.params_schema)();
    let payload = serde_json::json!({
        "name": descriptor.name,
        "description": descriptor.description,
        "impact": descriptor.impact,
        "capability": descriptor.capability,
        "effect": descriptor.effect,
        "base_authority": {
            "session_state": descriptor.base_authority.session_state,
            "fetch_hosts": descriptor.base_authority.fetch_hosts,
            "read_audit": descriptor.base_authority.read_audit,
        },
        "params_schema": params,
    });
    let canonical = lancet_protocol::canonical_json(&payload);
    let mut hasher = Sha256::new();
    hasher.update(NATIVE_DESCRIPTOR_DOMAIN);
    hasher.update((canonical.len() as u64).to_be_bytes());
    hasher.update(canonical.as_bytes());
    hex_digest(hasher.finalize().into())
}

fn hex_digest(digest: [u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Catalog method exposure resolved for a turn or embedding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolExposure {
    enabled_names: BTreeSet<String>,
    definitions: Vec<ToolDefinition>,
    base_authority: BaseAuthorityRequirement,
}

impl ToolExposure {
    /// Resolve enabled method names against the production CEP catalog.
    pub fn from_catalog_enabled(enabled: &[String]) -> Result<Self, ToolExposureError> {
        Self::from_descriptors(enabled, catalog())
    }

    /// Resolve enabled method names against catalog descriptors.
    pub fn from_descriptors(
        enabled: &[String],
        descriptors: &[MethodDescriptor],
    ) -> Result<Self, ToolExposureError> {
        let available: BTreeSet<String> = descriptors.iter().map(|d| d.name.to_string()).collect();
        let requested: BTreeSet<String> = enabled.iter().cloned().collect();

        let unknown: Vec<String> = requested.difference(&available).cloned().collect();
        if !unknown.is_empty() {
            return Err(ToolExposureError {
                unknown,
                available: available.into_iter().collect(),
            });
        }

        let mut base_authority = BaseAuthorityRequirement::NONE;
        let mut definitions = Vec::new();
        for descriptor in descriptors {
            if requested.contains(descriptor.name) {
                base_authority = base_authority.union(descriptor.base_authority);
                definitions.push(ToolDefinition::new(
                    descriptor.name,
                    descriptor.description,
                    (descriptor.params_schema)(),
                    native_descriptor_identity(descriptor),
                ));
            }
        }

        Ok(Self {
            enabled_names: requested,
            definitions,
            base_authority,
        })
    }

    /// Resolve enabled method names against a projected tool inventory.
    pub fn resolve(
        enabled: &[String],
        inventory: &[ToolDefinition],
    ) -> Result<Self, ToolExposureError> {
        let available: BTreeSet<String> = inventory.iter().map(|d| d.name.clone()).collect();
        let requested: BTreeSet<String> = enabled.iter().cloned().collect();

        let unknown: Vec<String> = requested.difference(&available).cloned().collect();
        if !unknown.is_empty() {
            return Err(ToolExposureError {
                unknown,
                available: available.into_iter().collect(),
            });
        }

        let definitions = inventory
            .iter()
            .filter(|d| requested.contains(&d.name))
            .cloned()
            .collect();

        Ok(Self {
            enabled_names: requested,
            definitions,
            base_authority: BaseAuthorityRequirement::NONE,
        })
    }

    /// Exact enabled CEP method names.
    pub fn enabled_names(&self) -> &BTreeSet<String> {
        &self.enabled_names
    }

    /// True if the method name is enabled for this exposure.
    pub fn contains(&self, name: &str) -> bool {
        self.enabled_names.contains(name)
    }

    /// Agent-visible tool definitions for the enabled set.
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    /// Base-envelope authority required by the enabled set.
    pub fn base_authority(&self) -> BaseAuthorityRequirement {
        self.base_authority
    }

    /// Consume into owned definitions.
    pub fn into_definitions(self) -> Vec<ToolDefinition> {
        self.definitions
    }
}

/// Unknown enabled-tool names against a known inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExposureError {
    pub unknown: Vec<String>,
    pub available: Vec<String>,
}

impl fmt::Display for ToolExposureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown enabled tools {:?}; available catalog tools: {:?}",
            self.unknown, self.available
        )
    }
}

impl Error for ToolExposureError {}

/// Project CEP method descriptors into LLM-facing tool definitions.
pub fn project_tool_definitions(catalog: &[MethodDescriptor]) -> Vec<ToolDefinition> {
    catalog
        .iter()
        .map(|descriptor| {
            ToolDefinition::new(
                descriptor.name,
                descriptor.description,
                (descriptor.params_schema)(),
                native_descriptor_identity(descriptor),
            )
        })
        .collect()
}

/// Build catalog-derived tool definitions filtered by enabled names.
pub fn resolve_catalog_tool_definitions(
    enabled: &[String],
) -> Result<Vec<ToolDefinition>, ToolExposureError> {
    ToolExposure::from_catalog_enabled(enabled).map(ToolExposure::into_definitions)
}

/// Filter an arbitrary tool inventory down to enabled names.
pub fn resolve_enabled_tools(
    enabled: &[String],
    inventory: &[ToolDefinition],
) -> Result<Vec<ToolDefinition>, ToolExposureError> {
    ToolExposure::resolve(enabled, inventory).map(ToolExposure::into_definitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definition_roundtrip() {
        let tool = ToolDefinition::new(
            "observe.workspace",
            "Observe workspace",
            serde_json::json!({"type": "object"}),
            "a".repeat(64),
        );
        let json = serde_json::to_string(&tool).unwrap();
        let deserialized: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(tool, deserialized);
    }

    #[test]
    fn native_descriptor_identity_is_stable_hex() {
        let projected = project_tool_definitions(catalog());
        assert!(!projected.is_empty());
        for tool in &projected {
            assert_eq!(tool.descriptor_identity.len(), 64);
            assert!(
                tool.descriptor_identity
                    .chars()
                    .all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "identity must be lowercase hex: {}",
                tool.descriptor_identity
            );
        }
        // Reprojection is byte-identical.
        let again = project_tool_definitions(catalog());
        assert_eq!(projected, again);
    }

    #[test]
    fn projection_length_equals_catalog_len() {
        let projected = project_tool_definitions(catalog());
        assert_eq!(projected.len(), catalog().len());
    }

    #[test]
    fn projected_names_match_catalog_exactly() {
        let projected = project_tool_definitions(catalog());
        for (tool, descriptor) in projected.iter().zip(catalog().iter()) {
            assert_eq!(tool.name, descriptor.name);
        }
    }

    #[test]
    fn enabled_filter_rejects_unknown() {
        let err = resolve_catalog_tool_definitions(&["does.not.exist".to_string()])
            .expect_err("unknown tool must error");
        assert_eq!(err.unknown, vec!["does.not.exist".to_string()]);
        assert!(!err.available.is_empty());
    }

    #[test]
    fn enabled_filter_uses_exact_names() {
        // This filter resolves only against `lancet_catalog::catalog()`. A
        // prefix of its remaining native method must still not resolve.
        let exposure = ToolExposure::from_catalog_enabled(&["fetch.web".to_string()])
            .expect("known tool resolves");
        assert!(exposure.contains("fetch.web"));
        assert!(!exposure.contains("fetch"));
        assert_eq!(exposure.definitions().len(), 1);
        assert_eq!(exposure.definitions()[0].name, "fetch.web");
    }

    /// Migrated Component Tools do not resolve through the native catalog.
    #[test]
    fn migrated_component_tools_do_not_resolve_through_the_catalog() {
        for migrated in [
            "observe.workspace",
            "mutate.create",
            "mutate.edit",
            "session.todo_write",
        ] {
            let err = ToolExposure::from_catalog_enabled(&[migrated.to_string()])
                .expect_err("a Component Tool is not a catalog method");
            assert_eq!(err.unknown, vec![migrated.to_string()]);
        }
    }

    #[test]
    fn catalog_exposure_collects_base_authority_from_descriptors() {
        let exposure = ToolExposure::from_catalog_enabled(&["fetch.web".to_string()])
            .expect("known tool resolves");
        let authority = exposure.base_authority();
        assert!(authority.fetch_hosts);
        assert!(!authority.session_state);
        assert!(!authority.read_audit);
    }

    #[test]
    fn inventory_only_exposure_has_no_authority_semantics() {
        let inventory = vec![ToolDefinition::new(
            "custom.tool",
            "Custom tool",
            serde_json::json!({"type": "object"}),
            "a".repeat(64),
        )];
        let exposure =
            ToolExposure::resolve(&["custom.tool".to_string()], &inventory).expect("tool resolves");
        assert_eq!(exposure.base_authority(), BaseAuthorityRequirement::NONE);
    }
}
