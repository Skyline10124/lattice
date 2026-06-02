//! Static CEP method catalog for CEP.

use crate::method::MethodDescriptor;
use crate::methods::{
    AuditObserve, AuditReplay, Create, Envelope, Fetch, ObserveWorkspace, Patch, Run, TodoWrite,
};
use std::sync::LazyLock;

/// Exhaustive CEP method descriptor list in stable registration order.
pub static CATALOG: LazyLock<[MethodDescriptor; 9]> = LazyLock::new(|| {
    [
        MethodDescriptor::for_method::<ObserveWorkspace>(),
        MethodDescriptor::for_method::<Create>(),
        MethodDescriptor::for_method::<Patch>(),
        MethodDescriptor::for_method::<Run>(),
        MethodDescriptor::for_method::<Fetch>(),
        MethodDescriptor::for_method::<TodoWrite>(),
        MethodDescriptor::for_method::<Envelope>(),
        MethodDescriptor::for_method::<AuditObserve>(),
        MethodDescriptor::for_method::<AuditReplay>(),
    ]
});

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
    use crate::methods::{
        AuditObserve, AuditReplay, Create, Envelope, Fetch, ObserveWorkspace, Patch, Run, TodoWrite,
    };
    use lancet_protocol::{ImpactKind, RequiredCapability};

    const EXPECTED_ORDER: [&str; 9] = [
        "observe.workspace",
        "mutate.create",
        "mutate.patch",
        "exec.command_run",
        "fetch.web",
        "session.todo_write",
        "session.envelope",
        "observe.audit",
        "observe.audit_replay",
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
    fn catalog_impact_and_capability_match_method_constants() {
        let catalog = catalog();
        let checks: [(&MethodDescriptor, ImpactKind, RequiredCapability); 9] = [
            (
                &catalog[0],
                ObserveWorkspace::IMPACT,
                ObserveWorkspace::CAPABILITY,
            ),
            (&catalog[1], Create::IMPACT, Create::CAPABILITY),
            (&catalog[2], Patch::IMPACT, Patch::CAPABILITY),
            (&catalog[3], Run::IMPACT, Run::CAPABILITY),
            (&catalog[4], Fetch::IMPACT, Fetch::CAPABILITY),
            (&catalog[5], TodoWrite::IMPACT, TodoWrite::CAPABILITY),
            (&catalog[6], Envelope::IMPACT, Envelope::CAPABILITY),
            (&catalog[7], AuditObserve::IMPACT, AuditObserve::CAPABILITY),
            (&catalog[8], AuditReplay::IMPACT, AuditReplay::CAPABILITY),
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
    fn catalog_len_is_9() {
        assert_eq!(catalog().len(), 9);
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
        let props = schema
            .schema
            .object
            .expect("object schema")
            .properties
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
        assert_eq!(catalog().len(), 9);
        let names: Vec<_> = catalog().iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            [
                "observe.workspace",
                "mutate.create",
                "mutate.patch",
                "exec.command_run",
                "fetch.web",
                "session.todo_write",
                "session.envelope",
                "observe.audit",
                "observe.audit_replay",
            ]
        );
        for descriptor in catalog() {
            assert_ne!(descriptor.name, "workspace.inspect");
            assert_ne!(descriptor.name, "workspace.navigate");
        }
    }

    #[cfg(feature = "inspect-scaffold")]
    #[test]
    fn catalog_default_snapshot_scaffold_is_separate() {
        assert_eq!(scaffold_catalog().len(), 2);
        assert!(!catalog()
            .iter()
            .any(|d| d.name == "workspace.inspect" || d.name == "workspace.navigate"));
    }
}
