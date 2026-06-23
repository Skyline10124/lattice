// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP CEP catalog method types.

pub mod audit;
pub mod command;
pub mod external;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;
pub mod session;
pub mod workspace;

pub use audit::{AuditObserve, AuditReplay};
pub use command::Run;
pub use external::Fetch;
pub use session::TodoWrite;
pub use workspace::{
    Batch, BatchOperation, BatchParams, BatchResult, Create, Edit, EditOperation, EditParams,
    EditResult, FindFilesParams, FindFilesResult, GrepParams, GrepResult, ListDirectoryParams,
    ListDirectoryResult, ObserveWorkspace, ReadFileParams, ReadFileResult,
};

#[cfg(feature = "inspect-scaffold")]
pub use scaffold::{Inspect, Navigate, ScaffoldStubResult};

mod sealed {
    use super::{
        audit::{AuditObserve, AuditReplay},
        command::Run,
        external::Fetch,
        session::TodoWrite,
        workspace::{Batch, Create, Edit, ObserveWorkspace},
    };
    use crate::method::sealed::Sealed;

    impl Sealed for ObserveWorkspace {}
    impl Sealed for Create {}
    impl Sealed for Edit {}
    impl Sealed for Batch {}
    impl Sealed for Fetch {}
    impl Sealed for TodoWrite {}
    impl Sealed for Run {}
    impl Sealed for AuditObserve {}
    impl Sealed for AuditReplay {}
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
            Batch::NAME,
            Fetch::NAME,
            TodoWrite::NAME,
            Run::NAME,
            AuditObserve::NAME,
            AuditReplay::NAME,
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
            Batch::NAME,
            Fetch::NAME,
            TodoWrite::NAME,
            Run::NAME,
            AuditObserve::NAME,
            AuditReplay::NAME,
        ];
        for name in names {
            assert_ne!(name, "session/create");
            assert_ne!(name, "tool/call");
            assert_ne!(name, "turn/start");
        }
    }
}
