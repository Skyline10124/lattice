//! Capability bundle resolution for built-in lattice tools.
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySelection {
    #[serde(default)]
    pub bundles: Vec<ToolBundle>,
    #[serde(default)]
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCapabilities {
    pub bundles: BTreeSet<ToolBundle>,
    pub tool_names: BTreeSet<String>,
    pub definitions: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
    #[error("unknown tool bundle: {0}")]
    UnknownBundle(String),
}

pub fn resolve_capabilities(selection: &CapabilitySelection) -> ResolvedCapabilities {
    let bundles: BTreeSet<ToolBundle> = selection.bundles.iter().copied().collect();
    let tool_names = resolve_tool_names(selection);
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

/// Returns the deterministic `BTreeSet<String>` union of all tools expanded
/// from selected bundles plus all explicit `enabled` tool names (D-29).
pub fn resolve_tool_names(selection: &CapabilitySelection) -> BTreeSet<String> {
    let mut tool_names = BTreeSet::new();

    for bundle in &selection.bundles {
        tool_names.extend(bundle.tool_names().into_iter().map(String::from));
    }

    for name in &selection.enabled {
        tool_names.insert(name.clone());
    }

    tool_names
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

#[cfg(test)]
mod tests {
    use super::*;

    fn names_for(selection: &CapabilitySelection) -> BTreeSet<String> {
        resolve_capabilities(selection).tool_names
    }

    // ── bundle-only selection ──────────────────────────────────────

    #[test]
    fn bundles_workspace_exposes_workspace_context() {
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Workspace],
            enabled: Vec::new(),
        };
        assert_eq!(
            names_for(&selection),
            BTreeSet::from(["workspace_context".to_string()])
        );
    }

    #[test]
    fn bundles_filesystem_exposes_read_and_search_tools() {
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Filesystem],
            enabled: Vec::new(),
        };
        let names = names_for(&selection);
        for tool in [
            "read_file",
            "read_files",
            "grep",
            "list_directory",
            "find_files",
        ] {
            assert!(
                names.contains(tool),
                "{tool} should be in filesystem bundle"
            );
        }
        assert!(!names.contains("write_file"));
    }

    #[test]
    fn bundles_editing_exposes_write_and_patch() {
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Editing],
            enabled: Vec::new(),
        };
        let names = names_for(&selection);
        assert!(names.contains("write_file"));
        assert!(names.contains("patch"));
    }

    #[test]
    fn bundles_shell_exposes_bash() {
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Shell],
            enabled: Vec::new(),
        };
        assert_eq!(names_for(&selection), BTreeSet::from(["bash".to_string()]));
    }

    #[test]
    fn bundles_planning_exposes_todo_write() {
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Planning],
            enabled: Vec::new(),
        };
        assert_eq!(
            names_for(&selection),
            BTreeSet::from(["todo_write".to_string()])
        );
    }

    #[test]
    fn bundles_web_exposes_web_search() {
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Web],
            enabled: Vec::new(),
        };
        assert_eq!(
            names_for(&selection),
            BTreeSet::from(["web_search".to_string()])
        );
    }

    #[test]
    fn coding_capable_bundle_set_exposes_coding_tools() {
        let selection = CapabilitySelection {
            bundles: vec![
                ToolBundle::Workspace,
                ToolBundle::Filesystem,
                ToolBundle::Editing,
                ToolBundle::Shell,
                ToolBundle::Planning,
                ToolBundle::Web,
            ],
            enabled: Vec::new(),
        };
        let resolved = resolve_capabilities(&selection);
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
    fn workspace_and_filesystem_bundles_exclude_editing_shell_planning_web() {
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Workspace, ToolBundle::Filesystem],
            enabled: Vec::new(),
        };
        let names = names_for(&selection);
        assert!(names.contains("workspace_context"));
        assert!(names.contains("read_file"));
        for excluded in ["write_file", "patch", "bash", "todo_write", "web_search"] {
            assert!(!names.contains(excluded), "{excluded} should be excluded");
        }
    }

    // ── explicit enabled tool names ─────────────────────────────────

    #[test]
    fn enabled_adds_explicit_tool_without_bundle() {
        let selection = CapabilitySelection {
            bundles: Vec::new(),
            enabled: vec!["grep".into()],
        };
        let names = names_for(&selection);
        assert!(names.contains("grep"));
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn enabled_with_bundle_union_includes_both() {
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Workspace],
            enabled: vec!["grep".into()],
        };
        let names = names_for(&selection);
        assert!(names.contains("workspace_context"));
        assert!(names.contains("grep"));
        assert_eq!(names.len(), 2);
    }

    // ── deduplication ───────────────────────────────────────────────

    #[test]
    fn duplicate_names_from_bundles_and_enabled_are_deduped() {
        // 'workspace_context' is already in the Workspace bundle; enable it
        // explicitly too — should still appear only once.
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Workspace],
            enabled: vec!["workspace_context".into()],
        };
        let names = names_for(&selection);
        assert_eq!(names, BTreeSet::from(["workspace_context".to_string()]));
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn duplicate_bundle_explicit_tool_dedup_is_deterministic() {
        // Run the same selection twice and verify the order and content match.
        let selection = CapabilitySelection {
            bundles: vec![ToolBundle::Filesystem],
            enabled: vec!["read_file".into()],
        };
        let a = names_for(&selection);
        let b = names_for(&selection);
        assert_eq!(a, b);
    }

    // ── default / empty selection ───────────────────────────────────

    #[test]
    fn empty_selection_resolves_no_tools() {
        let resolved = resolve_capabilities(&CapabilitySelection::default());
        assert!(resolved.bundles.is_empty());
        assert!(resolved.tool_names.is_empty());
        assert!(resolved.definitions.is_empty());
    }

    #[test]
    fn default_selection_is_empty() {
        let sel = CapabilitySelection::default();
        assert!(sel.bundles.is_empty());
        assert!(sel.enabled.is_empty());
    }
}
