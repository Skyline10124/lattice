//! Capability bundle and preset resolution for built-in lattice tools.
//!
//! This module owns tool exposure policy. `default_tool_definitions()` remains
//! the schema inventory; callers use capability selections here to decide which
//! of those schemas and tool names an agent is allowed to see and execute.

use std::collections::BTreeSet;
use std::str::FromStr;

use lattice_core::types::ToolDefinition;
use serde::{Deserialize, Serialize};

use crate::default_tool_definitions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolBundle {
    Core,
    Workspace,
    Filesystem,
    Editing,
    Shell,
    Planning,
    Web,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolPreset {
    Minimal,
    Reader,
    #[serde(alias = "code-agent")]
    Coding,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySelection {
    #[serde(default)]
    pub presets: Vec<ToolPreset>,
    #[serde(default)]
    pub bundles: Vec<ToolBundle>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCapabilities {
    pub bundles: BTreeSet<ToolBundle>,
    pub tool_names: BTreeSet<String>,
    pub definitions: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
    #[error("unknown tool preset: {0}")]
    UnknownPreset(String),
    #[error("unknown tool bundle: {0}")]
    UnknownBundle(String),
}

pub fn resolve_capabilities(selection: &CapabilitySelection) -> ResolvedCapabilities {
    let bundles = resolve_bundles(selection);
    let tool_names = tool_names_for_bundles(&bundles);
    let definitions = default_tool_definitions()
        .into_iter()
        .filter(|definition| tool_names.contains(&definition.name))
        .collect();

    ResolvedCapabilities {
        bundles,
        tool_names,
        definitions,
    }
}

pub fn resolve_tool_names(selection: &CapabilitySelection) -> BTreeSet<String> {
    resolve_capabilities(selection).tool_names
}

fn resolve_bundles(selection: &CapabilitySelection) -> BTreeSet<ToolBundle> {
    let mut bundles = BTreeSet::new();

    for preset in &selection.presets {
        bundles.extend(preset.bundles());
    }

    bundles.extend(selection.bundles.iter().copied());
    bundles
}

fn tool_names_for_bundles(bundles: &BTreeSet<ToolBundle>) -> BTreeSet<String> {
    let mut tool_names = BTreeSet::new();
    for bundle in bundles {
        tool_names.extend(bundle.tool_names().into_iter().map(String::from));
    }
    tool_names
}

impl ToolPreset {
    fn bundles(self) -> Vec<ToolBundle> {
        match self {
            ToolPreset::Minimal => Vec::new(),
            ToolPreset::Reader => vec![ToolBundle::Workspace, ToolBundle::Filesystem],
            ToolPreset::Coding => vec![
                ToolBundle::Workspace,
                ToolBundle::Filesystem,
                ToolBundle::Editing,
                ToolBundle::Shell,
                ToolBundle::Planning,
                ToolBundle::Web,
            ],
        }
    }
}

impl ToolBundle {
    pub(crate) fn tool_names(self) -> Vec<&'static str> {
        match self {
            ToolBundle::Core => {
                #[cfg(feature = "blob-store")]
                {
                    vec!["bus:fetch"]
                }

                #[cfg(not(feature = "blob-store"))]
                {
                    Vec::new()
                }
            }
            ToolBundle::Workspace => vec!["workspace_context"],
            ToolBundle::Filesystem => vec![
                "read_file",
                "read_files",
                "grep",
                "list_directory",
                "find_files",
            ],
            ToolBundle::Editing => vec!["write_file", "patch"],
            ToolBundle::Shell => vec!["bash"],
            ToolBundle::Planning => vec!["todo_write"],
            ToolBundle::Web => vec!["web_search"],
        }
    }

    pub(crate) fn contains_tool(self, tool_name: &str) -> bool {
        self.tool_names().into_iter().any(|name| name == tool_name)
    }
}

impl FromStr for ToolBundle {
    type Err = CapabilityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "core" => Ok(Self::Core),
            "workspace" => Ok(Self::Workspace),
            "filesystem" => Ok(Self::Filesystem),
            "editing" => Ok(Self::Editing),
            "shell" => Ok(Self::Shell),
            "planning" => Ok(Self::Planning),
            "web" => Ok(Self::Web),
            _ => Err(CapabilityError::UnknownBundle(value.to_string())),
        }
    }
}

impl FromStr for ToolPreset {
    type Err = CapabilityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "minimal" => Ok(Self::Minimal),
            "reader" => Ok(Self::Reader),
            "coding" | "code-agent" => Ok(Self::Coding),
            _ => Err(CapabilityError::UnknownPreset(value.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_for(selection: CapabilitySelection) -> BTreeSet<String> {
        resolve_capabilities(&selection).tool_names
    }

    #[test]
    fn coding_preset_resolves_expected_tools() {
        let resolved = resolve_capabilities(&CapabilitySelection {
            presets: vec![ToolPreset::Coding],
            bundles: Vec::new(),
        });

        assert_eq!(
            resolved.bundles,
            BTreeSet::from([
                ToolBundle::Workspace,
                ToolBundle::Filesystem,
                ToolBundle::Editing,
                ToolBundle::Shell,
                ToolBundle::Planning,
                ToolBundle::Web,
            ])
        );
        assert!(resolved.tool_names.contains("workspace_context"));
        assert!(resolved.tool_names.contains("read_file"));
        assert!(resolved.tool_names.contains("write_file"));
        assert!(resolved.tool_names.contains("patch"));
        assert!(resolved.tool_names.contains("bash"));
        assert!(resolved.tool_names.contains("todo_write"));
        assert!(resolved.tool_names.contains("web_search"));
        assert_eq!(resolved.definitions.len(), resolved.tool_names.len());
    }

    #[test]
    fn coding_preset_from_str_accepts_code_agent_alias() {
        assert_eq!(ToolPreset::from_str("coding"), Ok(ToolPreset::Coding));
        assert_eq!(ToolPreset::from_str("code-agent"), Ok(ToolPreset::Coding));
    }

    #[test]
    fn coding_preset_serde_roundtrips() {
        let serialized = serde_json::to_string(&ToolPreset::Coding).expect("serialize preset");

        assert_eq!(serialized, r#""coding""#);
        assert_eq!(
            serde_json::from_str::<ToolPreset>(&serialized).expect("deserialize preset"),
            ToolPreset::Coding
        );
        assert_eq!(
            serde_json::from_str::<ToolPreset>(r#""code-agent""#)
                .expect("deserialize legacy preset alias"),
            ToolPreset::Coding
        );
        assert_eq!(
            serde_json::from_str::<CapabilitySelection>(r#"{"presets":["code-agent"]}"#)
                .expect("deserialize legacy capability selection alias")
                .presets,
            vec![ToolPreset::Coding]
        );
    }

    #[test]
    fn reader_preset_excludes_editing_shell_planning_and_web() {
        let names = names_for(CapabilitySelection {
            presets: vec![ToolPreset::Reader],
            bundles: Vec::new(),
        });

        assert!(names.contains("workspace_context"));
        assert!(names.contains("read_file"));
        for excluded in ["write_file", "patch", "bash", "todo_write", "web_search"] {
            assert!(!names.contains(excluded), "{excluded} should be excluded");
        }
    }

    #[test]
    fn editing_bundle_exposes_patch() {
        let resolved = resolve_capabilities(&CapabilitySelection {
            presets: Vec::new(),
            bundles: vec![ToolBundle::Editing],
        });

        assert!(resolved.tool_names.contains("write_file"));
        assert!(resolved.tool_names.contains("patch"));
    }

    #[test]
    fn workspace_context_is_workspace_bundle() {
        let names = names_for(CapabilitySelection {
            presets: Vec::new(),
            bundles: vec![ToolBundle::Workspace],
        });

        assert_eq!(names, BTreeSet::from(["workspace_context".to_string()]));
    }

    #[test]
    fn minimal_preset_resolves_no_tools() {
        let resolved = resolve_capabilities(&CapabilitySelection {
            presets: vec![ToolPreset::Minimal],
            bundles: Vec::new(),
        });

        assert!(resolved.bundles.is_empty());
        assert!(resolved.tool_names.is_empty());
        assert!(resolved.definitions.is_empty());
    }
}
