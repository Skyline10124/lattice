//! Command physical-boundary vocabulary.
//!
//! These are stable wire/config labels. They describe what is being claimed,
//! not how native Linux primitives are applied.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// User/audit-facing command boundary dimensions.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryDimension {
    Network,
    Filesystem,
    Landlock,
}

impl BoundaryDimension {
    pub const ALL: &'static [BoundaryDimension] = &[
        BoundaryDimension::Network,
        BoundaryDimension::Filesystem,
        BoundaryDimension::Landlock,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            BoundaryDimension::Network => "network",
            BoundaryDimension::Filesystem => "filesystem",
            BoundaryDimension::Landlock => "landlock",
        }
    }
}

/// Host probe primitive labels.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryPrimitive {
    NetworkNamespace,
    MountNamespace,
    Overlayfs,
    Landlock,
}

impl BoundaryPrimitive {
    pub const ALL: &'static [BoundaryPrimitive] = &[
        BoundaryPrimitive::NetworkNamespace,
        BoundaryPrimitive::MountNamespace,
        BoundaryPrimitive::Overlayfs,
        BoundaryPrimitive::Landlock,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            BoundaryPrimitive::NetworkNamespace => "network_namespace",
            BoundaryPrimitive::MountNamespace => "mount_namespace",
            BoundaryPrimitive::Overlayfs => "overlayfs",
            BoundaryPrimitive::Landlock => "landlock",
        }
    }
}

/// Stable host probe status values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostProbeStatus {
    Available,
    Unavailable,
    Disabled,
    Unknown,
}

/// Stable per-command effective boundary status values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveBoundaryStatus {
    PhysicallyBounded,
    Unbounded,
    NotRequested,
    FailedClosed,
    Unsupported,
    Unknown,
}

/// One primitive result in a host capability probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostProbeEntry {
    pub primitive: BoundaryPrimitive,
    pub status: HostProbeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub diagnostics: BTreeMap<String, String>,
}

impl HostProbeEntry {
    pub fn new(
        primitive: BoundaryPrimitive,
        status: HostProbeStatus,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            primitive,
            status,
            reason: Some(reason.into()),
            diagnostics: BTreeMap::new(),
        }
    }

    pub fn with_diagnostic(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.diagnostics.insert(key.into(), value.into());
        self
    }
}

/// Primitive-keyed host capability report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostProbeReport {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub primitives: BTreeMap<BoundaryPrimitive, HostProbeEntry>,
}

impl HostProbeReport {
    pub fn from_entries(entries: impl IntoIterator<Item = HostProbeEntry>) -> Self {
        Self {
            primitives: entries
                .into_iter()
                .map(|entry| (entry.primitive, entry))
                .collect(),
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &HostProbeEntry> {
        self.primitives.values()
    }

    pub fn status_for(&self, primitive: BoundaryPrimitive) -> Option<&HostProbeEntry> {
        self.primitives.get(&primitive)
    }
}

/// One boundary result for a specific command execution attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectiveBoundaryEntry {
    pub boundary: BoundaryDimension,
    pub status: EffectiveBoundaryStatus,
    #[serde(default)]
    pub observed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub diagnostics: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_primitives: Vec<BoundaryPrimitive>,
}

impl EffectiveBoundaryEntry {
    pub fn new(boundary: BoundaryDimension, status: EffectiveBoundaryStatus) -> Self {
        Self {
            boundary,
            status,
            observed: false,
            reason: None,
            diagnostics: BTreeMap::new(),
            source_primitives: Vec::new(),
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn with_diagnostic(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.diagnostics.insert(key.into(), value.into());
        self
    }

    pub fn with_source_primitive(mut self, primitive: BoundaryPrimitive) -> Self {
        self.source_primitives.push(primitive);
        self
    }
}

/// Boundary-keyed per-command effective boundary report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectiveBoundaryReport {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub boundaries: BTreeMap<BoundaryDimension, EffectiveBoundaryEntry>,
}

impl EffectiveBoundaryReport {
    pub fn from_entries(entries: impl IntoIterator<Item = EffectiveBoundaryEntry>) -> Self {
        Self {
            boundaries: entries
                .into_iter()
                .map(|entry| (entry.boundary, entry))
                .collect(),
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &EffectiveBoundaryEntry> {
        self.boundaries.values()
    }

    pub fn status_for(&self, boundary: BoundaryDimension) -> Option<&EffectiveBoundaryEntry> {
        self.boundaries.get(&boundary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_dimension_wire_labels_are_stable() {
        let labels: Vec<_> = BoundaryDimension::ALL
            .iter()
            .map(|dimension| serde_json::to_string(dimension).unwrap().replace('"', ""))
            .collect();
        assert_eq!(labels, vec!["network", "filesystem", "landlock"]);
    }

    #[test]
    fn boundary_primitive_wire_labels_are_stable() {
        let labels: Vec<_> = BoundaryPrimitive::ALL
            .iter()
            .map(|primitive| serde_json::to_string(primitive).unwrap().replace('"', ""))
            .collect();
        assert_eq!(
            labels,
            vec![
                "network_namespace",
                "mount_namespace",
                "overlayfs",
                "landlock"
            ]
        );
    }

    #[test]
    fn host_probe_status_wire_labels_are_stable() {
        let labels: Vec<_> = [
            HostProbeStatus::Available,
            HostProbeStatus::Unavailable,
            HostProbeStatus::Disabled,
            HostProbeStatus::Unknown,
        ]
        .iter()
        .map(|status| serde_json::to_string(status).unwrap().replace('"', ""))
        .collect();
        assert_eq!(
            labels,
            vec!["available", "unavailable", "disabled", "unknown"]
        );
    }

    #[test]
    fn effective_boundary_status_wire_labels_are_stable() {
        let labels: Vec<_> = [
            EffectiveBoundaryStatus::PhysicallyBounded,
            EffectiveBoundaryStatus::Unbounded,
            EffectiveBoundaryStatus::NotRequested,
            EffectiveBoundaryStatus::FailedClosed,
            EffectiveBoundaryStatus::Unsupported,
            EffectiveBoundaryStatus::Unknown,
        ]
        .iter()
        .map(|status| serde_json::to_string(status).unwrap().replace('"', ""))
        .collect();
        assert_eq!(
            labels,
            vec![
                "physically_bounded",
                "unbounded",
                "not_requested",
                "failed_closed",
                "unsupported",
                "unknown"
            ]
        );
    }

    #[test]
    fn host_probe_report_json_is_primitive_keyed_with_diagnostics() {
        let report = HostProbeReport::from_entries([
            HostProbeEntry::new(
                BoundaryPrimitive::Overlayfs,
                HostProbeStatus::Available,
                "overlay smoke probe mounted",
            )
            .with_diagnostic("probe", "child_overlay_mount"),
            HostProbeEntry::new(
                BoundaryPrimitive::NetworkNamespace,
                HostProbeStatus::Unavailable,
                "unshare(CLONE_NEWNET) returned EPERM",
            )
            .with_diagnostic("errno", "EPERM"),
            HostProbeEntry::new(
                BoundaryPrimitive::MountNamespace,
                HostProbeStatus::Disabled,
                "user namespaces disabled",
            ),
            HostProbeEntry::new(
                BoundaryPrimitive::Landlock,
                HostProbeStatus::Unknown,
                "landlock ABI probe returned unexpected errno",
            ),
        ]);

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["primitives"]["overlayfs"]["status"],
            serde_json::json!("available")
        );
        assert_eq!(
            json["primitives"]["network_namespace"]["reason"],
            serde_json::json!("unshare(CLONE_NEWNET) returned EPERM")
        );
        assert_eq!(
            json["primitives"]["network_namespace"]["diagnostics"]["errno"],
            serde_json::json!("EPERM")
        );
        assert!(json["primitives"]["mount_namespace"].is_object());
        assert!(json["primitives"]["landlock"].is_object());
    }

    #[test]
    fn effective_boundary_report_json_is_boundary_keyed_not_primitive_keyed() {
        let report = EffectiveBoundaryReport::from_entries([
            EffectiveBoundaryEntry::new(
                BoundaryDimension::Network,
                EffectiveBoundaryStatus::PhysicallyBounded,
            )
            .with_reason("netns evidence accepted"),
            EffectiveBoundaryEntry::new(
                BoundaryDimension::Filesystem,
                EffectiveBoundaryStatus::FailedClosed,
            )
            .with_diagnostic("missing_primitive", "overlayfs"),
            EffectiveBoundaryEntry::new(
                BoundaryDimension::Landlock,
                EffectiveBoundaryStatus::Unsupported,
            ),
        ]);

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["boundaries"]["network"]["status"],
            serde_json::json!("physically_bounded")
        );
        assert_eq!(
            json["boundaries"]["filesystem"]["diagnostics"]["missing_primitive"],
            serde_json::json!("overlayfs")
        );
        assert!(json["boundaries"]["landlock"].is_object());
        assert!(json["boundaries"]["network_namespace"].is_null());
        assert!(json["boundaries"]["overlayfs"].is_null());
        assert!(json["boundaries"]["mount_namespace"].is_null());
    }

    #[test]
    fn observed_default_is_false_for_old_effective_boundary_entry_json() {
        let entry: EffectiveBoundaryEntry = serde_json::from_value(serde_json::json!({
            "boundary": "network",
            "status": "physically_bounded",
            "reason": "legacy audit row without observation data"
        }))
        .unwrap();

        assert_eq!(entry.boundary, BoundaryDimension::Network);
        assert_eq!(entry.status, EffectiveBoundaryStatus::PhysicallyBounded);
        assert!(!entry.observed);

        let json = serde_json::to_value(EffectiveBoundaryEntry::new(
            BoundaryDimension::Filesystem,
            EffectiveBoundaryStatus::PhysicallyBounded,
        ))
        .unwrap();

        assert_eq!(json["observed"], serde_json::json!(false));
        assert!(json.get("observed_dimensions").is_none());
        assert!(json.get("unobserved_dimensions").is_none());
    }

    #[test]
    fn boundary_observed_status_and_observation_are_independent() {
        let network = EffectiveBoundaryEntry::new(
            BoundaryDimension::Network,
            EffectiveBoundaryStatus::PhysicallyBounded,
        );
        let mut filesystem = EffectiveBoundaryEntry::new(
            BoundaryDimension::Filesystem,
            EffectiveBoundaryStatus::PhysicallyBounded,
        );
        filesystem.observed = true;

        let report = EffectiveBoundaryReport::from_entries([network, filesystem]);
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(
            json["boundaries"]["network"]["status"],
            serde_json::json!("physically_bounded")
        );
        assert_eq!(
            json["boundaries"]["network"]["observed"],
            serde_json::json!(false)
        );
        assert_eq!(
            json["boundaries"]["filesystem"]["status"],
            serde_json::json!("physically_bounded")
        );
        assert_eq!(
            json["boundaries"]["filesystem"]["observed"],
            serde_json::json!(true)
        );

        for key in [
            "workspace_read_set",
            "tree_out_read",
            "observed_dimensions",
            "unobserved_dimensions",
        ] {
            assert!(json.get(key).is_none(), "{key} must not be a report field");
            assert!(
                json["boundaries"]["network"].get(key).is_none(),
                "{key} must not be an entry field"
            );
            assert!(
                json["boundaries"]["filesystem"].get(key).is_none(),
                "{key} must not be an entry field"
            );
        }
    }

    #[test]
    fn boundary_observed_schema_exposes_typed_entry_field() {
        let schema = schemars::schema_for!(EffectiveBoundaryEntry);
        let json = serde_json::to_value(&schema).unwrap();
        let observed = json
            .pointer("/properties/observed")
            .or_else(|| json.pointer("/schema/properties/observed"))
            .expect("observed is present in EffectiveBoundaryEntry schema");

        assert_eq!(observed["type"], serde_json::json!("boolean"));
    }
}
