// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Workspace mutation methods.
//!
//! `observe.workspace` used to live here. #30 migrated it to an embedded
//! Component Tool, so its method type, its four mode params/result pairs
//! (`ReadFile*`, `Grep*`, `ListDirectory*`, `FindFiles*`) and their
//! `CepMethod`/`CatalogMethod` impls are gone: `tools/observe-workspace/
//! manifest.toml` is now the sole descriptor and schema authority for the
//! method. Re-adding any of them here would be the parallel schema ADR 0004
//! forbids, and a name in both vocabularies fails the server run closed
//! (`validate_no_component_native_collision`).
//!
//! The workspace-facing Rust types the Component's *host* side needs
//! (`DirectoryEntry`, `GrepMatch`, …) live in
//! `lancet_runtime::workspace::types`, behind the WIT Tool Host imports —
//! not in this crate's catalog vocabulary.

use crate::method::{CatalogMethod, CepMethod};
use lancet_protocol::RequiredCapability;
use lancet_protocol::{
    EffectDecl, EffectKindSet, EffectScopeKind, FailureSemantics, ImpactKind, OverlayBehavior,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
        // `observe.workspace` is absent since #30: the name it would contribute
        // here is now asserted against `tools/observe-workspace/manifest.toml`,
        // not against a Rust `CepMethod` this crate defines.
        for name in [Create::NAME, Edit::NAME] {
            assert!(name.contains('.'), "expected dotted name: {name}");
            assert!(!name.contains('/'), "slash names forbidden: {name}");
        }
    }

    /// The hash handoff is a two-sided contract and #30 moved only one side of
    /// it out. `observe.workspace`'s "do not compute, transform, or narrate the
    /// hash" half now lives in the Component manifest description; the half this
    /// crate still owns is `mutate.edit` telling the agent where the value comes
    /// from. Without this, `expected_hash` reads as a field the agent may
    /// compute itself, which is exactly the failure the instruction prevents.
    #[test]
    fn edit_description_sources_expected_hash_from_the_observation_tool() {
        assert!(Edit::DESCRIPTION.contains("observe.workspace content_hash as expected_hash"));
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
