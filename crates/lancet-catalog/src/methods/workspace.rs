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
    const DESCRIPTION: &'static str = "Observe workspace content in one of four fixed modes: read_file for one path, grep for regex search under a root, list_directory for one directory, or find_files for glob-like path search under a root.";
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CreateResult {
    pub path: String,
    pub bytes_written: u64,
}

impl CepMethod for Create {
    const NAME: &'static str = "mutate.create";
    const DESCRIPTION: &'static str = "Create a new file under the declared workspace scope. The target file must not already exist, and parent directories must already exist.";
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

/// `mutate.patch`
pub struct Patch;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PatchParams {
    pub patch: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PatchResult {
    pub changed_files: Vec<String>,
}

impl CepMethod for Patch {
    const NAME: &'static str = "mutate.patch";
    const DESCRIPTION: &'static str = "Apply LATTICE hashline patch text with multi-file update and delete operations. Patch text must use the exact envelope format: *** Begin Patch, then one or more *** Update File: path / *** Delete File: path operations, then *** End Patch. Every operation requires *** Old Hash: blake3:<64 hex> from observe.workspace read_file content_hash. Hunk context/removal lines after @@ must carry the current line anchor as '<prefix>blake3:<4 hex><tab>text' using a leading space for context or - for removals; the short anchor is the first 4 hex digits of BLAKE3 over the before-state 1-based line number and line text. Additions use +text. New files must use mutate.create. Do not send git unified diff syntax such as --- a/file or +++ b/file.";
    type Params = PatchParams;
    type Result = PatchResult;
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

impl CatalogMethod for Patch {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::CepMethod;

    #[test]
    fn workspace_method_names_are_full_dotted_names() {
        for name in [ObserveWorkspace::NAME, Create::NAME, Patch::NAME] {
            assert!(name.contains('.'), "expected dotted name: {name}");
            assert!(!name.contains('/'), "slash names forbidden: {name}");
        }
    }

    #[test]
    fn read_file_params_schema_has_path_property() {
        let schema = schemars::schema_for!(ReadFileParams);
        let value = serde_json::to_value(schema).unwrap();
        assert!(value["properties"]["path"].is_object());
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
}
