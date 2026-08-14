// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Repository identity record at `refs/lattice/meta` (ADR 0005 §4).
//!
//! Every kernel-managed repository carries one fixed identity ref:
//!
//! ```text
//! refs/lattice/meta -> commit(tree{ meta.json })  — canonical JSON:
//! {
//!     "repository_format_version": 1,        // kernel envelope version
//!     "store_kind": "session",               // domain-owned
//!     "store_schema_version": 1,             // domain-owned
//!     "scope": {"type":"global"} | {"type":"project","key":"<ProjectStoreKey>"}
//! }
//! ```
//!
//! There is intentionally no `StoreInstanceId`: the design does not promise to
//! detect an operator who deletes a whole store and installs a different
//! schema-valid empty repository at the same location.

use serde::{Deserialize, Serialize};

use crate::{ProjectStoreKey, StoreError};

/// Kernel envelope version written into every `refs/lattice/meta`.
pub const REPOSITORY_FORMAT_VERSION: u32 = 1;

/// The fixed identity ref name.
pub const META_REF: &str = "refs/lattice/meta";

/// File inside the meta commit's tree carrying the canonical JSON record.
pub const META_TREE_FILE: &str = "meta.json";

/// Store scope: a global repository (exactly one per state root) or a
/// project-scoped repository bound to its directory's [`ProjectStoreKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum StoreScope {
    /// Global scope (`sessions.git`).
    Global,
    /// Project scope; the key must equal the key of the directory the
    /// repository lives in.
    Project {
        /// The `ProjectStoreKey` of the containing project directory.
        key: ProjectStoreKey,
    },
}

/// The domain-supplied identity record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreMeta {
    /// Domain-owned store kind (e.g. `"session"`, `"workspace-history"`).
    pub store_kind: String,
    /// Domain-owned schema version of the store's contents.
    pub store_schema_version: u32,
    /// Global or project scope.
    pub scope: StoreScope,
}

/// What an opener expects the identity record to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedMeta {
    /// Required store kind.
    pub store_kind: String,
    /// Required schema version.
    pub store_schema_version: u32,
    /// Required scope (for project stores: the directory's key).
    pub scope: StoreScope,
}

impl From<&ExpectedMeta> for StoreMeta {
    fn from(expected: &ExpectedMeta) -> Self {
        Self {
            store_kind: expected.store_kind.clone(),
            store_schema_version: expected.store_schema_version,
            scope: expected.scope,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetaJson {
    repository_format_version: u32,
    store_kind: String,
    store_schema_version: u32,
    scope: StoreScope,
}

/// Serialize the identity record to canonical bytes (the content of
/// `meta.json` in the meta commit's tree).
pub fn serialize_meta(meta: &StoreMeta) -> Result<Vec<u8>, StoreError> {
    if meta.store_kind.is_empty()
        || !meta
            .store_kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(StoreError::Git(format!(
            "store kind {:?} must be non-empty ascii alphanumerics and '-'",
            meta.store_kind
        )));
    }
    crate::canon::canonical_json_bytes(&MetaJson {
        repository_format_version: REPOSITORY_FORMAT_VERSION,
        store_kind: meta.store_kind.clone(),
        store_schema_version: meta.store_schema_version,
        scope: meta.scope,
    })
}

/// Parse and validate identity bytes read from a repository.
///
/// Every mismatch is typed corruption; open never repairs, quarantines, or
/// substitutes an empty record.
pub fn parse_meta(bytes: &[u8]) -> Result<StoreMeta, StoreError> {
    let parsed: MetaJson = serde_json::from_slice(bytes).map_err(|e| StoreError::Corruption {
        detail: format!("{META_REF}: invalid meta.json: {e}"),
    })?;
    if parsed.repository_format_version != REPOSITORY_FORMAT_VERSION {
        return Err(StoreError::Corruption {
            detail: format!(
                "{META_REF}: unsupported repository_format_version {} (kernel supports {REPOSITORY_FORMAT_VERSION})",
                parsed.repository_format_version
            ),
        });
    }
    Ok(StoreMeta {
        store_kind: parsed.store_kind,
        store_schema_version: parsed.store_schema_version,
        scope: parsed.scope,
    })
}

/// Validate a parsed identity against what the opener expects.
pub fn check_expected(meta: &StoreMeta, expected: &ExpectedMeta) -> Result<(), StoreError> {
    if meta.store_kind != expected.store_kind {
        return Err(StoreError::Corruption {
            detail: format!(
                "{META_REF}: wrong store kind '{}' (expected '{}')",
                meta.store_kind, expected.store_kind
            ),
        });
    }
    if meta.store_schema_version != expected.store_schema_version {
        return Err(StoreError::Corruption {
            detail: format!(
                "{META_REF}: unsupported store_schema_version {} for kind '{}' (expected {})",
                meta.store_schema_version, expected.store_kind, expected.store_schema_version
            ),
        });
    }
    if meta.scope != expected.scope {
        return Err(StoreError::Corruption {
            detail: format!(
                "{META_REF}: scope mismatch (repository records {:?}, expected {:?})",
                meta.scope, expected.scope
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> StoreMeta {
        StoreMeta {
            store_kind: "session".into(),
            store_schema_version: 1,
            scope: StoreScope::Global,
        }
    }

    #[test]
    fn canonical_wire_bytes_are_stable() {
        let bytes = serialize_meta(&meta()).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"repository_format_version":1,"scope":{"type":"global"},"store_kind":"session","store_schema_version":1}"#
        );
    }

    #[test]
    fn project_scope_wire_form() {
        let key = ProjectStoreKey::from_canonical_path_bytes(b"/ws");
        let bytes = serialize_meta(&StoreMeta {
            store_kind: "workspace-history".into(),
            store_schema_version: 1,
            scope: StoreScope::Project { key },
        })
        .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(r#""scope":{"key":""#));
        assert!(text.contains(r#""type":"project""#));
    }

    #[test]
    fn round_trip_and_expected_check() {
        let meta = meta();
        let parsed = parse_meta(&serialize_meta(&meta).unwrap()).unwrap();
        assert_eq!(parsed, meta);
        check_expected(
            &parsed,
            &ExpectedMeta {
                store_kind: "session".into(),
                store_schema_version: 1,
                scope: StoreScope::Global,
            },
        )
        .unwrap();
    }

    #[test]
    fn rejects_wrong_kind_version_scope_and_format() {
        let meta = meta();
        for expected in [
            ExpectedMeta {
                store_kind: "other".into(),
                ..ExpectedMeta::from(&meta)
            },
            ExpectedMeta {
                store_schema_version: 2,
                ..ExpectedMeta::from(&meta)
            },
            ExpectedMeta {
                scope: StoreScope::Project {
                    key: ProjectStoreKey::from_canonical_path_bytes(b"/ws"),
                },
                ..ExpectedMeta::from(&meta)
            },
        ] {
            assert!(check_expected(&meta, &expected).is_err());
        }

        let mut bad: serde_json::Value =
            serde_json::from_slice(&serialize_meta(&meta).unwrap()).unwrap();
        bad["repository_format_version"] = serde_json::json!(99);
        assert!(parse_meta(&serde_json::to_vec(&bad).unwrap()).is_err());
    }

    #[test]
    fn rejects_unknown_fields_and_garbage() {
        assert!(parse_meta(b"not json").is_err());
        assert!(parse_meta(br#"{"repository_format_version":1,"store_kind":"s","store_schema_version":1,"scope":{"type":"global"},"extra":1}"#).is_err());
    }

    impl From<&StoreMeta> for ExpectedMeta {
        fn from(meta: &StoreMeta) -> Self {
            Self {
                store_kind: meta.store_kind.clone(),
                store_schema_version: meta.store_schema_version,
                scope: meta.scope,
            }
        }
    }
}
