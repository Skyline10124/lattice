use lattice_tools::{
    default_tool_definitions, resolve_capabilities, CapabilitySelection, ToolBundle, ToolPreset,
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
fn editing_bundle_exposes_patch_without_multi_patch() {
    let resolved = resolve_capabilities(&CapabilitySelection {
        presets: Vec::new(),
        bundles: vec![ToolBundle::Editing],
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
fn coding_preset_exposes_patch_without_multi_patch() {
    let resolved = resolve_capabilities(&CapabilitySelection {
        presets: vec![ToolPreset::Coding],
        bundles: Vec::new(),
    });

    assert!(resolved.tool_names.contains("write_file"));
    assert!(resolved.tool_names.contains("patch"));
    assert!(!resolved.tool_names.contains("multi_patch"));
}

#[test]
fn non_coding_profiles_do_not_expose_coding_tools() {
    let resolved = resolve_capabilities(&CapabilitySelection {
        presets: vec![ToolPreset::Reader],
        bundles: Vec::new(),
    });
    let coding = resolve_capabilities(&CapabilitySelection {
        presets: vec![ToolPreset::Coding],
        bundles: Vec::new(),
    });

    for excluded in ["patch", "bash", "todo_write", "write_file"] {
        assert!(
            coding.tool_names.contains(excluded),
            "{excluded} should be a coding preset tool"
        );
        assert!(
            !resolved.tool_names.contains(excluded),
            "{excluded} leaked into reader preset"
        );
        assert!(
            resolved
                .definitions
                .iter()
                .all(|definition| definition.name != excluded),
            "{excluded} schema leaked into reader preset"
        );
    }
}
