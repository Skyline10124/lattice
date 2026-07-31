// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP CEP catalog method types.

pub mod external;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;
pub mod workspace;

pub use external::Fetch;
// `ObserveWorkspace` and its four mode params/result types are gone (#30): the
// method is an embedded Component Tool and `tools/observe-workspace/manifest.toml`
// is its sole contract authority.
pub use workspace::{Create, Edit, EditOperation, EditParams, EditResult};

#[cfg(feature = "inspect-scaffold")]
pub use scaffold::{Inspect, Navigate, ScaffoldStubResult};

mod sealed {
    use super::{
        external::Fetch,
        workspace::{Create, Edit},
    };
    use crate::method::sealed::Sealed;

    impl Sealed for Create {}
    impl Sealed for Edit {}
    impl Sealed for Fetch {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::CepMethod;

    /// The three native methods this crate still defines. `observe.workspace`
    /// (#30) and `session.todo_write` (#29) are absent because they are
    /// Component Tools with no `CepMethod` type left to name.
    const NATIVE_METHOD_NAMES: [&str; 3] = [Create::NAME, Edit::NAME, Fetch::NAME];

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
