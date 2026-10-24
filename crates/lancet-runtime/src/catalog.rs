// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-visible projection of the generated closed Component catalog.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ComponentRuntime;

/// One generated Component descriptor projected for a model provider.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
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

/// Unknown enabled Tool names against the immutable generated catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSelectionError {
    pub unknown: Vec<String>,
    pub available: Vec<String>,
}

impl fmt::Display for CatalogSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown enabled tools {:?}; available catalog tools: {:?}",
            self.unknown, self.available
        )
    }
}

impl Error for CatalogSelectionError {}

/// Project and filter the generated descriptors in one step.
pub fn enabled_tool_definitions(
    runtime: &ComponentRuntime,
    enabled: &[String],
) -> Result<Vec<ToolDefinition>, CatalogSelectionError> {
    let requested: BTreeSet<String> = enabled.iter().cloned().collect();
    let available: BTreeSet<String> = runtime
        .descriptors()
        .map(|descriptor| descriptor.name.clone())
        .collect();
    let unknown: Vec<String> = requested.difference(&available).cloned().collect();
    if !unknown.is_empty() {
        return Err(CatalogSelectionError {
            unknown,
            available: available.into_iter().collect(),
        });
    }

    Ok(runtime
        .descriptor_identities()
        .filter(|(descriptor, _)| requested.contains(&descriptor.name))
        .map(|(descriptor, identity)| {
            ToolDefinition::new(
                descriptor.name.clone(),
                descriptor.description.clone(),
                descriptor.params_schema.clone(),
                identity,
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_projection_rejects_unknown_names() {
        let runtime = ComponentRuntime::noop_fixture().unwrap();
        let error = enabled_tool_definitions(&runtime, &["unknown__tool".into()]).unwrap_err();
        assert_eq!(error.unknown, vec!["unknown__tool"]);
        assert_eq!(error.available, vec!["noop__echo"]);
    }
}
