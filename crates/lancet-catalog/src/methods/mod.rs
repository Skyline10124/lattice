//! CEP CEP catalog method types.

pub mod audit;
pub mod command;
pub mod envelope;
pub mod external;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;
pub mod session;
pub mod workspace;

pub use audit::{AuditObserve, AuditReplay};
pub use command::Run;
pub use envelope::Envelope;
pub use external::Fetch;
pub use session::TodoWrite;
pub use workspace::{
    Create, FindFilesParams, FindFilesResult, GrepParams, GrepResult, ListDirectoryParams,
    ListDirectoryResult, ObserveWorkspace, Patch, ReadFileParams, ReadFileResult,
};

#[cfg(feature = "inspect-scaffold")]
pub use scaffold::{Inspect, Navigate, ScaffoldStubResult};

mod sealed {
    use super::{
        audit::{AuditObserve, AuditReplay},
        command::Run,
        envelope::Envelope,
        external::Fetch,
        session::TodoWrite,
        workspace::{Create, ObserveWorkspace, Patch},
    };
    use crate::method::sealed::Sealed;

    impl Sealed for ObserveWorkspace {}
    impl Sealed for Create {}
    impl Sealed for Patch {}
    impl Sealed for Fetch {}
    impl Sealed for TodoWrite {}
    impl Sealed for Run {}
    impl Sealed for Envelope {}
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
            Fetch::NAME,
            TodoWrite::NAME,
            Run::NAME,
            Patch::NAME,
            Envelope::NAME,
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
            Fetch::NAME,
            TodoWrite::NAME,
            Run::NAME,
            Patch::NAME,
            Envelope::NAME,
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
