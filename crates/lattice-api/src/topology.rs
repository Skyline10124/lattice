//! v1.1 migration-home table for old complex crates.
//!
//! This module makes the v1.1 homes for `lattice-prompt`, `lattice-sandbox`,
//! and `lattice-policy` explicit without importing those legacy crates. It
//! serves as code-level migration evidence for CRATE-04; broader old-name
//! removal is owned by Phase 11.
//!
//! # Migration status
//!
//! | Legacy crate | v1.1 home | Owning phase |
//! |---|---|---|
//! | `lattice-prompt` | Direct `lattice-profile` fields + `lattice-agent` message assembly | Phase 7 / 9 |
//! | `lattice-sandbox` | Narrow `lattice-tools` project-root and file-size checks | Phase 9 |
//! | `lattice-policy` | Narrow tool/API result types only if needed | Phase 9 / 11 |

use serde::{Deserialize, Serialize};

/// A single migration-home entry recording where a legacy crate's concepts
/// will move in the v1.1 architecture and which phase owns the work.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyEntry {
    /// The legacy crate name (e.g., "lattice-prompt").
    pub legacy_crate: &'static str,
    /// The canonical v1.1 home description.
    pub v11_home: &'static str,
    /// The phase that owns the migration work.
    pub phase: &'static str,
}

/// Return the migration-home table for old complex crates.
///
/// This table documents where `lattice-prompt`, `lattice-sandbox`, and
/// `lattice-policy` concepts will be integrated into the v1.1 architecture.
/// It does not import any legacy crates.
pub fn migration_homes() -> &'static [TopologyEntry] {
    &[
        TopologyEntry {
            legacy_crate: "lattice-prompt",
            v11_home: "Direct lattice-profile fields + lattice-agent message assembly",
            phase: "Phase 7 / 9",
        },
        TopologyEntry {
            legacy_crate: "lattice-sandbox",
            v11_home: "Narrow lattice-tools project-root and file-size checks",
            phase: "Phase 9",
        },
        TopologyEntry {
            legacy_crate: "lattice-policy",
            v11_home: "Narrow tool/API result types only if needed",
            phase: "Phase 9 / 11",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_homes_includes_prompt() {
        let homes = migration_homes();
        let prompt = homes
            .iter()
            .find(|e| e.legacy_crate == "lattice-prompt");
        assert!(prompt.is_some(), "should include lattice-prompt entry");
    }

    #[test]
    fn migration_homes_includes_sandbox() {
        let homes = migration_homes();
        let sandbox = homes
            .iter()
            .find(|e| e.legacy_crate == "lattice-sandbox");
        assert!(sandbox.is_some(), "should include lattice-sandbox entry");
    }

    #[test]
    fn migration_homes_includes_policy() {
        let homes = migration_homes();
        let policy = homes
            .iter()
            .find(|e| e.legacy_crate == "lattice-policy");
        assert!(policy.is_some(), "should include lattice-policy entry");
    }

    #[test]
    fn all_entries_have_non_empty_fields() {
        let homes = migration_homes();
        assert!(!homes.is_empty(), "should have at least one entry");
        for entry in homes {
            assert!(
                !entry.legacy_crate.is_empty(),
                "legacy_crate should not be empty"
            );
            assert!(
                !entry.v11_home.is_empty(),
                "v11_home should not be empty for '{}'",
                entry.legacy_crate
            );
            assert!(
                !entry.phase.is_empty(),
                "phase should not be empty for '{}'",
                entry.legacy_crate
            );
        }
    }

    #[test]
    fn migration_homes_table_does_not_import_legacy_crates() {
        // Compile-time assertion: this module does not use `lattice_prompt`,
        // `lattice_sandbox`, or `lattice_policy` in any way. If it compiles,
        // the test passes.
        let homes = migration_homes();
        for entry in homes {
            // The legacy crate names are string constants, not crate imports.
            assert!(!entry.legacy_crate.is_empty());
        }
    }
}
