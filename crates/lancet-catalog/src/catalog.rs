// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Static CEP method catalog for CEP.

use crate::method::MethodDescriptor;
use crate::methods::{Create, Edit, Fetch, ObserveWorkspace, TodoWrite};
use std::sync::LazyLock;

/// Exhaustive CEP method descriptor list in stable registration order.
pub static CATALOG: LazyLock<[MethodDescriptor; 5]> = LazyLock::new(|| {
    [
        MethodDescriptor::for_method::<ObserveWorkspace>(),
        MethodDescriptor::for_method::<Create>(),
        MethodDescriptor::for_method::<Edit>(),
        MethodDescriptor::for_method::<Fetch>(),
        MethodDescriptor::for_method::<TodoWrite>(),
    ]
});

/// Compile-time-known length of [`CATALOG`]. Mirrors the array bound in
/// [`CATALOG`]'s type (`[MethodDescriptor; 5]`); keep these in sync when the
/// root CEP method contract changes. Callers (e.g. the LANCET doctor) read
/// this instead of hard-coding a method count.
pub const CATALOG_LEN: usize = 5;

/// Borrow the catalog as a slice.
pub fn catalog() -> &'static [MethodDescriptor] {
    &*CATALOG
}

#[cfg(feature = "inspect-scaffold")]
use crate::methods::scaffold::{Inspect, Navigate};

/// Feature-gated scaffold descriptors (`workspace.inspect`, `workspace.navigate`).
///
/// Never merged into [`catalog`] / [`CATALOG`]; use only via [`scaffold_catalog`].
#[cfg(feature = "inspect-scaffold")]
pub static SCAFFOLD_CATALOG: LazyLock<[MethodDescriptor; 2]> = LazyLock::new(|| {
    [
        MethodDescriptor::for_method::<Inspect>(),
        MethodDescriptor::for_method::<Navigate>(),
    ]
});

/// Borrow the inspect/navigate scaffold catalog (requires `inspect-scaffold` feature).
#[cfg(feature = "inspect-scaffold")]
pub fn scaffold_catalog() -> &'static [MethodDescriptor] {
    &*SCAFFOLD_CATALOG
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::CepMethod;
    use crate::methods::{Create, Edit, Fetch, ObserveWorkspace, TodoWrite};
    use lancet_protocol::{ImpactKind, RequiredCapability};

    const EXPECTED_ORDER: [&str; 5] = [
        "observe.workspace",
        "mutate.create",
        "mutate.edit",
        "fetch.web",
        "session.todo_write",
    ];

    #[test]
    fn catalog_contains_expected_methods_in_stable_order() {
        let names: Vec<_> = catalog().iter().map(|d| d.name).collect();
        assert_eq!(names, EXPECTED_ORDER);
    }

    #[test]
    fn catalog_method_names_are_unique() {
        let mut names: Vec<_> = catalog().iter().map(|d| d.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), catalog().len());
    }

    #[test]
    fn catalog_method_names_are_full_dotted_names() {
        for descriptor in catalog() {
            assert!(
                descriptor.name.contains('.'),
                "expected dotted name: {}",
                descriptor.name
            );
            assert!(
                !descriptor.name.contains('/'),
                "slash names forbidden: {}",
                descriptor.name
            );
        }
    }

    #[test]
    fn catalog_descriptors_have_non_empty_descriptions() {
        for descriptor in catalog() {
            assert!(
                !descriptor.description.is_empty(),
                "empty description for {}",
                descriptor.name
            );
        }
    }

    #[test]
    fn catalog_descriptors_have_params_schema_object() {
        for descriptor in catalog() {
            let schema = (descriptor.params_schema)();
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "params schema for {} must be an object",
                descriptor.name
            );
        }
    }

    #[test]
    fn catalog_descriptors_declare_base_authority_requirements() {
        for descriptor in catalog() {
            match descriptor.name {
                "session.todo_write" => {
                    assert!(descriptor.base_authority.session_state);
                    assert!(!descriptor.base_authority.fetch_hosts);
                    assert!(!descriptor.base_authority.read_audit);
                }
                "fetch.web" => {
                    assert!(!descriptor.base_authority.session_state);
                    assert!(descriptor.base_authority.fetch_hosts);
                    assert!(!descriptor.base_authority.read_audit);
                }
                "observe.workspace" | "mutate.create" | "mutate.edit" => {
                    assert_eq!(
                        descriptor.base_authority,
                        crate::method::BaseAuthorityRequirement::NONE
                    )
                }
                other => panic!("unexpected catalog method: {other}"),
            }
        }
    }

    #[test]
    fn catalog_impact_and_capability_match_method_constants() {
        let catalog = catalog();
        let checks: [(&MethodDescriptor, ImpactKind, RequiredCapability); 5] = [
            (
                &catalog[0],
                ObserveWorkspace::IMPACT,
                ObserveWorkspace::CAPABILITY,
            ),
            (&catalog[1], Create::IMPACT, Create::CAPABILITY),
            (&catalog[2], Edit::IMPACT, Edit::CAPABILITY),
            (&catalog[3], Fetch::IMPACT, Fetch::CAPABILITY),
            (&catalog[4], TodoWrite::IMPACT, TodoWrite::CAPABILITY),
        ];

        for (descriptor, impact, capability) in checks {
            assert_eq!(
                descriptor.impact, impact,
                "impact mismatch for {}",
                descriptor.name
            );
            assert_eq!(
                descriptor.capability, capability,
                "capability mismatch for {}",
                descriptor.name
            );
        }
    }

    #[test]
    fn catalog_len_is_5() {
        assert_eq!(catalog().len(), 5);
    }

    #[test]
    fn default_catalog_has_no_inspect_or_navigate() {
        for descriptor in catalog() {
            assert_ne!(descriptor.name, "workspace.inspect");
            assert_ne!(descriptor.name, "workspace.navigate");
        }
    }
}

#[cfg(all(test, feature = "inspect-scaffold"))]
mod scaffold_tests {
    use super::*;
    use crate::methods::scaffold::ScaffoldStubResult;
    use schemars::schema_for;

    const SCAFFOLD_ORDER: [&str; 2] = ["workspace.inspect", "workspace.navigate"];

    #[test]
    fn scaffold_catalog_contains_inspect_and_navigate() {
        let names: Vec<_> = scaffold_catalog().iter().map(|d| d.name).collect();
        assert_eq!(names, SCAFFOLD_ORDER);
    }

    #[test]
    fn scaffold_descriptors_have_object_params_schema() {
        for descriptor in scaffold_catalog() {
            let schema = (descriptor.params_schema)();
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "params schema for {} must be an object",
                descriptor.name,
            );
        }
    }

    #[test]
    fn scaffold_result_schema_includes_status_semantic_stage_and_routed() {
        let schema = schema_for!(ScaffoldStubResult);
        let value = serde_json::to_value(schema).expect("serialize schema");
        let props = value["properties"]
            .as_object()
            .expect("object schema with properties")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for field in ["status", "semantic_stage", "routed"] {
            assert!(
                props.contains(&field.to_string()),
                "ScaffoldStubResult missing {field}"
            );
        }
    }
}

#[cfg(test)]
mod catalog_default_snapshot {
    use super::*;

    #[test]
    fn catalog_default_snapshot_unchanged_with_all_features() {
        assert_eq!(catalog().len(), 5);
        let names: Vec<_> = catalog().iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            [
                "observe.workspace",
                "mutate.create",
                "mutate.edit",
                "fetch.web",
                "session.todo_write",
            ]
        );
        for descriptor in catalog() {
            assert_ne!(descriptor.name, "workspace.inspect");
            assert_ne!(descriptor.name, "workspace.navigate");
            assert_ne!(descriptor.name, "observe.audit");
            assert_ne!(descriptor.name, "observe.audit_replay");
        }
    }

    /// Regression test for issue #11: audit methods are operator-read only.
    #[test]
    fn catalog_excludes_audit_agent_methods() {
        for name in ["observe.audit", "observe.audit_replay"] {
            assert!(
                !catalog().iter().any(|d| d.name == name),
                "{name} must not appear in the production agent catalog"
            );
        }
    }

    /// Regression test for issue #4: `exec.command_run` is permanently retired
    /// from the CEP catalog. The catalog must contain exactly 5 methods and
    /// must NOT include `exec.command_run`.
    #[test]
    fn catalog_retires_exec_command_run() {
        assert_eq!(catalog().len(), CATALOG_LEN);
        assert!(
            !catalog().iter().any(|d| d.name == "exec.command_run"),
            "exec.command_run must not appear in the production catalog"
        );
    }

    /// Regression test for issue #15: `mutate.batch` is permanently retired from
    /// the CEP catalog.
    #[test]
    fn catalog_retires_mutate_batch() {
        assert_eq!(catalog().len(), CATALOG_LEN);
        assert!(
            !catalog().iter().any(|d| d.name == "mutate.batch"),
            "mutate.batch must not appear in the production catalog"
        );
    }

    /// `CATALOG_LEN` must stay in sync with the runtime catalog length so the
    /// LANCET doctor's expected-count check cannot silently drift.
    #[test]
    fn catalog_len_matches_runtime_catalog() {
        assert_eq!(CATALOG_LEN, catalog().len());
    }

    #[cfg(feature = "inspect-scaffold")]
    #[test]
    fn catalog_default_snapshot_scaffold_is_separate() {
        assert_eq!(scaffold_catalog().len(), 2);
        assert!(
            !catalog()
                .iter()
                .any(|d| d.name == "workspace.inspect" || d.name == "workspace.navigate")
        );
    }
}
