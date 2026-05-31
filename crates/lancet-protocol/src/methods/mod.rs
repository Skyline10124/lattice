//! CEP CEP catalog method types.

pub mod audit;
pub mod command;
pub mod envelope;
pub mod external;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;
pub mod session;
pub mod workspace;

pub use audit::{AuditQuery, AuditReplay, AuditTail};
pub use command::Run;
pub use envelope::{Declare, SubmitIntent};
pub use external::Fetch;
pub use session::TodoWrite;
pub use workspace::{FindFiles, Grep, ListDirectory, Patch, ReadFile, WriteFile};

#[cfg(feature = "inspect-scaffold")]
pub use scaffold::{Inspect, Navigate, ScaffoldStubResult};

mod sealed {
    use super::{
        audit::{AuditQuery, AuditReplay, AuditTail},
        command::Run,
        envelope::{Declare, SubmitIntent},
        external::Fetch,
        session::TodoWrite,
        workspace::{FindFiles, Grep, ListDirectory, Patch, ReadFile, WriteFile},
    };
    use crate::method::sealed::Sealed;

    impl Sealed for ReadFile {}
    impl Sealed for Grep {}
    impl Sealed for ListDirectory {}
    impl Sealed for FindFiles {}
    impl Sealed for WriteFile {}
    impl Sealed for Patch {}
    impl Sealed for Fetch {}
    impl Sealed for TodoWrite {}
    impl Sealed for Run {}
    impl Sealed for Declare {}
    impl Sealed for SubmitIntent {}
    impl Sealed for AuditQuery {}
    impl Sealed for AuditTail {}
    impl Sealed for AuditReplay {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::CepMethod;

    #[test]
    fn all_catalog_method_names_are_dotted_without_slashes() {
        let names = [
            ReadFile::NAME,
            Grep::NAME,
            ListDirectory::NAME,
            FindFiles::NAME,
            Fetch::NAME,
            TodoWrite::NAME,
            Run::NAME,
            WriteFile::NAME,
            Patch::NAME,
            Declare::NAME,
            SubmitIntent::NAME,
            AuditQuery::NAME,
            AuditTail::NAME,
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
            ReadFile::NAME,
            Grep::NAME,
            ListDirectory::NAME,
            FindFiles::NAME,
            Fetch::NAME,
            TodoWrite::NAME,
            Run::NAME,
            WriteFile::NAME,
            Patch::NAME,
            Declare::NAME,
            SubmitIntent::NAME,
            AuditQuery::NAME,
            AuditTail::NAME,
            AuditReplay::NAME,
        ];
        for name in names {
            assert_ne!(name, "session/create");
            assert_ne!(name, "tool/call");
            assert_ne!(name, "turn/start");
        }
    }
}
