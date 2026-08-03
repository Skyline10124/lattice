// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP CEP catalog method types.

pub mod external;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;
pub mod workspace;

pub use external::Fetch;
// `ObserveWorkspace` (#30), `Create`, and `Edit` (#31) are gone: those methods
// are embedded Component Tools and their source manifests are the sole contract
// authorities (`tools/observe-workspace/manifest.toml`,
// `tools/mutate-create/manifest.toml`, `tools/mutate-edit/manifest.toml`).

#[cfg(feature = "inspect-scaffold")]
pub use scaffold::{Inspect, Navigate, ScaffoldStubResult};

mod sealed {
    use super::external::Fetch;
    use crate::method::sealed::Sealed;

    impl Sealed for Fetch {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::CepMethod;

    /// The one native method this crate still defines. `observe.workspace`
    /// (#30), `session.todo_write` (#29), and `mutate.create`/`mutate.edit`
    /// (#31) are absent because they are Component Tools with no `CepMethod`
    /// type left to name.
    const NATIVE_METHOD_NAMES: [&str; 1] = [Fetch::NAME];

    #[test]
    fn all_catalog_method_names_are_dotted_without_slashes() {
        for name in NATIVE_METHOD_NAMES {
            assert!(name.contains('.'), "expected dotted name: {name}");
            assert!(!name.contains('/'), "slash names forbidden: {name}");
        }
    }

    #[test]
    fn no_legacy_slash_method_names_in_protocol() {
        for name in NATIVE_METHOD_NAMES {
            assert_ne!(name, "session/create");
            assert_ne!(name, "tool/call");
            assert_ne!(name, "turn/start");
        }
    }
}
