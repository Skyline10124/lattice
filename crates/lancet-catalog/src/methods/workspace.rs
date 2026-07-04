// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Workspace observation and mutation methods.

use crate::method::{CatalogMethod, CepMethod};
use lancet_protocol::RequiredCapability;
use lancet_protocol::{
    EffectDecl, EffectKindSet, EffectScopeKind, FailureSemantics, ImpactKind, OverlayBehavior,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `observe.workspace`
pub struct ObserveWorkspace;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ObserveWorkspaceParams {
    ReadFile(ReadFileParams),
    Grep(GrepParams),
    ListDirectory(ListDirectoryParams),
    FindFiles(FindFilesParams),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", content = "result", rename_all = "snake_case")]
pub enum ObserveWorkspaceResult {
    ReadFile(ReadFileResult),
    Grep(GrepResult),
    ListDirectory(ListDirectoryResult),
    FindFiles(FindFilesResult),
}

impl CepMethod for ObserveWorkspace {
    const NAME: &'static str = "observe.workspace";
    const DESCRIPTION: &'static str = "Observe workspace content in one of four fixed modes: read_file for one path, grep for regex search under a root, list_directory for one directory, or find_files for glob-like path search under a root. read_file returns content_hash; copy that exact value into mutate.edit expected_hash when editing, and do not compute, transform, or narrate the hash.";
    type Params = ObserveWorkspaceParams;
    type Result = ObserveWorkspaceResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::Inspect;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
        scope: EffectScopeKind::ObservationRoot,
        capabilities: &[RequiredCapability::ReadPath, RequiredCapability::Inspect],
        overlay: OverlayBehavior::ReadWorkspace,
        failure: FailureSemantics::NoBackendEffectOnFailure,
    };
}

impl CatalogMethod for ObserveWorkspace {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_numbers: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileResult {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GrepParams {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GrepMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GrepResult {
    pub matches: Vec<GrepMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListDirectoryParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: DirectoryEntryKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListDirectoryResult {
    pub entries: Vec<DirectoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindFilesParams {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindFilesResult {
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// `mutate.create`
pub struct Create;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CreateParams {
    pub path: String,
    pub content: String,
    /// Create missing parent directories needed for `path`. When false, all
    /// parent directories must already exist.
    #[serde(default)]
    pub create_parents: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CreateResult {
    pub path: String,
    pub bytes_written: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_dirs: Vec<String>,
}

impl CepMethod for Create {
    const NAME: &'static str = "mutate.create";
    const DESCRIPTION: &'static str = "Create a new file under the workspace write scope. The target path must be permitted by policy: use a 'dir/**' pattern to cover a directory. The target file must not already exist. Set create_parents=true to create missing parent directories required for path; otherwise parent directories must already exist. Directory creation is audited and bounded by the workspace write scope.";
    type Params = CreateParams;
    type Result = CreateResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceMutation;
    const CAPABILITY: RequiredCapability = RequiredCapability::WritePath;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]),
        scope: EffectScopeKind::WorkspaceWrite,
        capabilities: &[RequiredCapability::WritePath],
        overlay: OverlayBehavior::WriteWorkspace,
        failure: FailureSemantics::NoBackendEffectOnFailure,
    };
}

impl CatalogMethod for Create {}

/// `mutate.edit`
pub struct Edit;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditParams {
    pub path: String,
    /// Content hash returned by `observe.workspace` for the file state the edit
    /// was planned against. The runtime verifies this value and computes all
    /// hashline anchors internally.
    pub expected_hash: String,
    pub edits: Vec<EditOperation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOperation {
    ReplaceText {
        old: String,
        new: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        occurrence: Option<u64>,
    },
    InsertBefore {
        anchor: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        occurrence: Option<u64>,
    },
    InsertAfter {
        anchor: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        occurrence: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditResult {
    pub changed_files: Vec<String>,
}

impl CepMethod for Edit {
    const NAME: &'static str = "mutate.edit";
    const DESCRIPTION: &'static str = "Edit an existing file with semantic text operations. The target path must be permitted by policy: use a 'dir/**' pattern to cover a directory (a pattern without /** matches only one exact file). Provide the path, the observe.workspace content_hash as expected_hash, and an edits array where every operation object includes op plus its required fields: replace_text uses old/new, insert_before and insert_after use anchor/content. Put multiple changes to the same file in one mutate.edit edits array. LANCET computes hashline anchors internally. This method never deletes files.";
    type Params = EditParams;
    type Result = EditResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceMutation;
    const CAPABILITY: RequiredCapability = RequiredCapability::WritePath;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]),
        scope: EffectScopeKind::WorkspaceWrite,
        capabilities: &[RequiredCapability::WritePath],
        overlay: OverlayBehavior::WriteWorkspace,
        failure: FailureSemantics::RetainDiffOnVerifyFailure,
    };
}

impl CatalogMethod for Edit {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::CepMethod;

    #[test]
    fn workspace_method_names_are_full_dotted_names() {
        for name in [ObserveWorkspace::NAME, Create::NAME, Edit::NAME] {
            assert!(name.contains('.'), "expected dotted name: {name}");
            assert!(!name.contains('/'), "slash names forbidden: {name}");
        }
    }

    #[test]
    fn read_file_params_schema_has_path_property() {
        let schema = schemars::schema_for!(ReadFileParams);
        let value = serde_json::to_value(schema).unwrap();
        assert!(value["properties"]["path"].is_object());
        assert!(ObserveWorkspace::DESCRIPTION.contains("do not compute"));
    }

    #[test]
    fn read_file_round_trips() {
        let params = ReadFileParams {
            path: "src/main.rs".into(),
            offset: Some(1),
            limit: Some(100),
            line_numbers: Some(true),
        };
        let json = serde_json::to_string(&params).unwrap();
        let back: ReadFileParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back, params);
    }

    #[test]
    fn create_params_defaults_create_parents_to_false() {
        let params: CreateParams =
            serde_json::from_value(serde_json::json!({"path": "src/lib.rs", "content": ""}))
                .unwrap();
        assert!(!params.create_parents);
    }

    #[test]
    fn create_params_schema_exposes_create_parents() {
        let schema = schemars::schema_for!(CreateParams);
        let value = serde_json::to_value(schema).unwrap();
        assert!(value["properties"]["create_parents"].is_object());
        assert!(Create::DESCRIPTION.contains("create_parents=true"));
    }

    #[test]
    fn mutation_descriptions_point_to_write_scope_and_directory_patterns() {
        // Each mutating method must tell the agent to use 'dir/**' for
        // directories so it does not discover the requirement by trial
        // and error. Direct-mode policy enforces the write scope; there
        // is no envelope to declare.
        for desc in [Create::DESCRIPTION, Edit::DESCRIPTION] {
            assert!(
                desc.contains("/**"),
                "missing directory pattern guidance: {desc}"
            );
        }
    }
}
