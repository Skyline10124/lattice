// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP CEP catalog method types.

pub mod external;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;
pub mod workspace;

pub use external::Fetch;
pub use workspace::{
    Create, Edit, EditOperation, EditParams, EditResult, FindFilesParams, FindFilesResult,
    GrepParams, GrepResult, ListDirectoryParams, ListDirectoryResult, ObserveWorkspace,
    ReadFileParams, ReadFileResult,
};

#[cfg(feature = "inspect-scaffold")]
pub use scaffold::{Inspect, Navigate, ScaffoldStubResult};

mod sealed {
    use super::{
        external::Fetch,
        workspace::{Create, Edit, ObserveWorkspace},
    };
    use crate::method::sealed::Sealed;

    impl Sealed for ObserveWorkspace {}
    impl Sealed for Create {}
    impl Sealed for Edit {}
    impl Sealed for Fetch {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::CepMethod;

    #[test]
    fn all_catalog_method_names_are_dotted_without_slashes() {
        let names = [
            ObserveWorkspace::NAME,
            Create::NAME,
            Edit::NAME,
            Fetch::NAME,
        ];
        for name in names {
            assert!(name.contains('.'), "expected dotted name: {name}");
            assert!(!name.contains('/'), "slash names forbidden: {name}");
        }
    }

    #[test]
    fn no_legacy_slash_method_names_in_protocol() {
        let names = [
            ObserveWorkspace::NAME,
            Create::NAME,
            Edit::NAME,
            Fetch::NAME,
        ];
        for name in names {
            assert_ne!(name, "session/create");
            assert_ne!(name, "tool/call");
            assert_ne!(name, "turn/start");
        }
    }
}
