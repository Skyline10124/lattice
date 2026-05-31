//! Workspace observation and mutation methods.

use crate::method::{CatalogMethod, CepMethod};
use lattice_cep_protocol::RequiredCapability;
use lattice_cep_protocol::{EffectDecl, EffectKindSet, EffectScopeKind, ImpactKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `workspace.read_file`
pub struct ReadFile;

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

impl CepMethod for ReadFile {
    const NAME: &'static str = "workspace.read_file";
    const DESCRIPTION: &'static str =
        "Read a text file with optional 1-based line offset/limit and line-number output.";
    type Params = ReadFileParams;
    type Result = ReadFileResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::ReadPath;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
        scope: EffectScopeKind::PathRead,
        capabilities: &[RequiredCapability::ReadPath],
    };
}

impl CatalogMethod for ReadFile {}

/// `workspace.grep`
pub struct Grep;

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

impl CepMethod for Grep {
    const NAME: &'static str = "workspace.grep";
    const DESCRIPTION: &'static str =
        "Search for a regex pattern in files under a path. Results may be capped.";
    type Params = GrepParams;
    type Result = GrepResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::Inspect;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
        scope: EffectScopeKind::ObservationRoot,
        capabilities: &[RequiredCapability::Inspect],
    };
}

impl CatalogMethod for Grep {}

/// `workspace.list_directory`
pub struct ListDirectory;

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

impl CepMethod for ListDirectory {
    const NAME: &'static str = "workspace.list_directory";
    const DESCRIPTION: &'static str =
        "List files and directories in a path with stable sorting and output limits.";
    type Params = ListDirectoryParams;
    type Result = ListDirectoryResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::Inspect;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
        scope: EffectScopeKind::ObservationRoot,
        capabilities: &[RequiredCapability::Inspect],
    };
}

impl CatalogMethod for ListDirectory {}

/// `workspace.find_files`
pub struct FindFiles;

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

impl CepMethod for FindFiles {
    const NAME: &'static str = "workspace.find_files";
    const DESCRIPTION: &'static str =
        "Find files or directories by glob-like pattern. Results are stable and capped.";
    type Params = FindFilesParams;
    type Result = FindFilesResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::Inspect;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
        scope: EffectScopeKind::ObservationRoot,
        capabilities: &[RequiredCapability::Inspect],
    };
}

impl CatalogMethod for FindFiles {}

/// `workspace.write_file`
pub struct WriteFile;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WriteFileParams {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WriteFileResult {
    pub path: String,
    pub bytes_written: u64,
}

impl CepMethod for WriteFile {
    const NAME: &'static str = "workspace.write_file";
    const DESCRIPTION: &'static str = "Write content to a file under the declared workspace scope.";
    type Params = WriteFileParams;
    type Result = WriteFileResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceMutation;
    const CAPABILITY: RequiredCapability = RequiredCapability::WritePath;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]),
        scope: EffectScopeKind::WorkspaceWrite,
        capabilities: &[RequiredCapability::WritePath],
    };
}

impl CatalogMethod for WriteFile {}

/// `workspace.patch`
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
    const NAME: &'static str = "workspace.patch";
    const DESCRIPTION: &'static str =
        "Apply structured patch text with multi-file add, update, and delete operations.";
    type Params = PatchParams;
    type Result = PatchResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceMutation;
    const CAPABILITY: RequiredCapability = RequiredCapability::WritePath;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]),
        scope: EffectScopeKind::WorkspaceWrite,
        capabilities: &[RequiredCapability::WritePath],
    };
}

impl CatalogMethod for Patch {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::CepMethod;

    #[test]
    fn workspace_method_names_are_full_dotted_names() {
        for name in [
            ReadFile::NAME,
            Grep::NAME,
            ListDirectory::NAME,
            FindFiles::NAME,
            WriteFile::NAME,
            Patch::NAME,
        ] {
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
