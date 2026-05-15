use std::collections::BTreeSet;

use lattice_tools::{
    default_tool_definitions, resolve_capabilities, CapabilitySelection, ToolBundle,
};

#[test]
fn patch_schema_accepts_only_structured_patch_text() {
    let definitions = default_tool_definitions();
    let patch = definitions
        .iter()
        .find(|definition| definition.name == "patch")
        .expect("patch tool definition");

    assert_eq!(patch.parameters["required"], serde_json::json!(["patch"]));
    assert!(patch.parameters["properties"]["patch"].is_object());
    assert!(patch.parameters["properties"]["file_path"].is_null());
    assert!(patch.parameters["properties"]["search"].is_null());
    assert!(patch.parameters["properties"]["insert"].is_null());
}

#[test]
fn default_tool_definitions_do_not_include_multi_patch() {
    let definitions = default_tool_definitions();

    assert!(definitions
        .iter()
        .all(|definition| definition.name != "multi_patch"));
}

#[test]
fn bundles_enabled_editing_exposes_patch_without_multi_patch() {
    let resolved = resolve_capabilities(&CapabilitySelection {
        bundles: vec![ToolBundle::Editing],
        enabled: Vec::new(),
    });

    assert!(resolved.tool_names.contains("write_file"));
    assert!(resolved.tool_names.contains("patch"));
    assert!(!resolved.tool_names.contains("multi_patch"));
    assert!(resolved
        .definitions
        .iter()
        .all(|definition| definition.name != "multi_patch"));
}

#[test]
fn bundles_enabled_coding_capable_set_exposes_all_coding_tools() {
    let resolved = resolve_capabilities(&CapabilitySelection {
        bundles: vec![
            ToolBundle::Workspace,
            ToolBundle::Filesystem,
            ToolBundle::Editing,
            ToolBundle::Shell,
            ToolBundle::Planning,
            ToolBundle::Web,
        ],
        enabled: Vec::new(),
    });

    for tool in [
        "workspace_context",
        "read_file",
        "grep",
        "write_file",
        "patch",
        "bash",
        "todo_write",
        "web_search",
    ] {
        assert!(
            resolved.tool_names.contains(tool),
            "{tool} should be in coding-capable bundle set"
        );
    }
}

#[test]
fn bundles_enabled_workspace_and_filesystem_exclude_editing_shell_planning_web() {
    let coding = resolve_capabilities(&CapabilitySelection {
        bundles: vec![
            ToolBundle::Workspace,
            ToolBundle::Filesystem,
            ToolBundle::Editing,
            ToolBundle::Shell,
            ToolBundle::Planning,
            ToolBundle::Web,
        ],
        enabled: Vec::new(),
    });
    let read_only = resolve_capabilities(&CapabilitySelection {
        bundles: vec![ToolBundle::Workspace, ToolBundle::Filesystem],
        enabled: Vec::new(),
    });

    let read_only_names: BTreeSet<String> = read_only.tool_names;

    for excluded in ["patch", "bash", "todo_write", "write_file"] {
        assert!(
            coding.tool_names.contains(excluded),
            "{excluded} should be a coding-capable tool"
        );
        assert!(
            !read_only_names.contains(excluded),
            "{excluded} leaked into workspace+filesystem bundles"
        );
        assert!(
            read_only
                .definitions
                .iter()
                .all(|definition| definition.name != excluded),
            "{excluded} schema leaked into workspace+filesystem bundles"
        );
    }
}

#[test]
fn bundles_enabled_explicit_tool_without_bundle() {
    let resolved = resolve_capabilities(&CapabilitySelection {
        bundles: Vec::new(),
        enabled: vec!["grep".into()],
    });

    assert!(resolved.tool_names.contains("grep"));
    assert_eq!(resolved.tool_names.len(), 1);
}

#[test]
fn bundles_enabled_deduplicated_when_overlapping_with_bundle() {
    let resolved = resolve_capabilities(&CapabilitySelection {
        bundles: vec![ToolBundle::Filesystem],
        enabled: vec!["read_file".into()],
    });

    let count = resolved
        .tool_names
        .iter()
        .filter(|n| *n == "read_file")
        .count();
    assert_eq!(count, 1, "read_file should appear exactly once");
}

/// Source guard: forbid legacy preset symbols in production code.
#[test]
fn source_guard_no_presets_in_production() {
    let src = include_str!("../src/capabilities.rs");
    let lib = include_str!("../src/lib.rs");
    let combined = format!("{src}\n{lib}");

    let forbidden = [
        "ToolPreset",
        "presets:",
        "UnknownPreset",
        "code-agent",
        "coding_preset",
        "reader_preset",
        "minimal_preset",
    ];

    let test_start = combined.find("#[cfg(test)]").unwrap_or(combined.len());
    let prod = &combined[..test_start];

    for pattern in forbidden {
        if prod.contains(pattern) {
            panic!("production code must not contain '{pattern}'");
        }
    }
}
