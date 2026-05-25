//! Phase 19 CEP catalog method types.

pub mod command;
pub mod envelope;
pub mod external;
pub mod session;
pub mod workspace;

pub use command::Run;
pub use envelope::Declare;
pub use external::Fetch;
pub use session::TodoWrite;
pub use workspace::{FindFiles, Grep, ListDirectory, Patch, ReadFile, WriteFile};

mod sealed {
    use super::{
        command::Run,
        envelope::Declare,
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
        ];
        for name in names {
            assert_ne!(name, "session/create");
            assert_ne!(name, "tool/call");
            assert_ne!(name, "turn/start");
        }
    }
}
