// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_store::{RefExpectation, StateLayout};

use super::*;
use crate::SessionError;
use crate::git_graph::GitSessionGraphStore;
use crate::git_graph::helpers::{build_root_tree, build_turn_tree, parse_oid};

const SESSION: &str = "manifest-checkpoint-session";
const TURN: &str = "11111111-1111-4111-8111-111111111111";

#[derive(Clone, Copy, Debug)]
enum Constructor {
    Open,
    Create,
    OpenLayout,
    CreateLayout,
}

impl Constructor {
    fn open(
        self,
        path: &std::path::Path,
        layout: &StateLayout,
    ) -> crate::Result<GitSessionGraphStore> {
        match self {
            Self::Open => GitSessionGraphStore::open(path),
            Self::Create => GitSessionGraphStore::create(path),
            Self::OpenLayout => GitSessionGraphStore::open_layout(layout),
            Self::CreateLayout => GitSessionGraphStore::create_layout(layout),
        }
    }
}

fn forge_malformed_finalized_manifest(path: &std::path::Path) {
    let store = GitSessionGraphStore::create(path).unwrap();
    store
        .create_session(SESSION, None, "coder", "main", &test_project_key())
        .unwrap();
    let committed = store
        .test_commit_turn(commit_input(
            SESSION,
            Vec::new(),
            vec![user_msg("hello"), assistant_msg("done")],
            (),
        ))
        .unwrap();
    let original = parse_oid(&committed.commit_id).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &store
            .kernel()
            .read_tree_blob(original, "turn/manifest.json")
            .unwrap(),
    )
    .unwrap();
    manifest["workspace_history_checkpoint"] = serde_json::json!({"oid": "f".repeat(64)});
    let manifest_bytes = crate::graph::canonical_json_bytes(&manifest);
    let messages = String::from_utf8(
        store
            .kernel()
            .read_tree_blob(original, "turn/messages.jsonl")
            .unwrap(),
    )
    .unwrap();
    let agent = String::from_utf8(
        store
            .kernel()
            .read_tree_blob(original, "turn/agent.json")
            .unwrap(),
    )
    .unwrap();
    let profile = String::from_utf8(
        store
            .kernel()
            .read_tree_blob(original, "turn/profile.json")
            .unwrap(),
    )
    .unwrap();
    let turn_tree =
        build_turn_tree(store.kernel(), &manifest_bytes, &messages, &agent, &profile).unwrap();
    let root_tree = build_root_tree(store.kernel(), turn_tree).unwrap();
    let forged = store
        .kernel()
        .write_commit_at(root_tree, &[], "forged finalized turn", 1)
        .unwrap();
    store
        .kernel()
        .cas_ref(
            &format!("refs/lattice/sessions/{SESSION}/turns/{TURN}"),
            forged,
            RefExpectation::MustNotExist,
            "forge malformed finalized manifest",
        )
        .unwrap();
    assert!(
        store
            .kernel()
            .ref_target(&format!("refs/lattice/sessions/{SESSION}/active-turn"))
            .unwrap()
            .is_none(),
        "the corruption fixture must represent post-finalization state"
    );
}

#[test]
fn every_existing_store_constructor_rejects_a_malformed_finalized_checkpoint() {
    for constructor in [
        Constructor::Open,
        Constructor::Create,
        Constructor::OpenLayout,
        Constructor::CreateLayout,
    ] {
        let fixture = temp_dir("malformed-finalized-checkpoint");
        let layout = StateLayout::in_state_root(fixture.path());
        let path = match constructor {
            Constructor::Open | Constructor::Create => fixture.path().join("sessions.git"),
            Constructor::OpenLayout | Constructor::CreateLayout => {
                layout.lattice_root().join("sessions.git")
            }
        };
        forge_malformed_finalized_manifest(&path);
        let error = constructor
            .open(&path, &layout)
            .expect_err("strict construction must parse and validate the finalized manifest");
        assert!(
            matches!(error, SessionError::JournalCorruption { .. }),
            "{constructor:?} returned {error:?}"
        );
    }
}
