//! Static CEP method catalog for Phase 19.

use crate::method::MethodDescriptor;
use crate::methods::{
    Declare, Fetch, FindFiles, Grep, ListDirectory, Patch, ReadFile, Run, TodoWrite, WriteFile,
};
use std::sync::LazyLock;

/// Exhaustive Phase 19 method descriptor list in stable registration order.
pub static CATALOG: LazyLock<[MethodDescriptor; 10]> = LazyLock::new(|| {
    [
        MethodDescriptor::for_method::<ReadFile>(),
        MethodDescriptor::for_method::<Grep>(),
        MethodDescriptor::for_method::<ListDirectory>(),
        MethodDescriptor::for_method::<FindFiles>(),
        MethodDescriptor::for_method::<Fetch>(),
        MethodDescriptor::for_method::<TodoWrite>(),
        MethodDescriptor::for_method::<Run>(),
        MethodDescriptor::for_method::<WriteFile>(),
        MethodDescriptor::for_method::<Patch>(),
        MethodDescriptor::for_method::<Declare>(),
    ]
});

/// Borrow the catalog as a slice.
pub fn catalog() -> &'static [MethodDescriptor] {
    &*CATALOG
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::ImpactKind;
    use crate::method::{CepMethod, RequiredCapability};
    use crate::methods::{
        Declare, Fetch, FindFiles, Grep, ListDirectory, Patch, ReadFile, Run, TodoWrite, WriteFile,
    };

    const EXPECTED_ORDER: [&str; 10] = [
        "workspace.read_file",
        "workspace.grep",
        "workspace.list_directory",
        "workspace.find_files",
        "web.fetch",
        "session.todo_write",
        "command.run",
        "workspace.write_file",
        "workspace.patch",
        "envelope.declare",
    ];

    #[test]
    fn catalog_contains_expected_methods_in_stable_order() {
        let names: Vec<_> = catalog().iter().map(|d| d.name).collect();
        assert_eq!(names, EXPECTED_ORDER);
    }

    #[test]
    fn catalog_method_names_are_unique() {
        let mut names: Vec<_> = catalog().iter().map(|d| d.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), catalog().len());
    }

    #[test]
    fn catalog_method_names_are_full_dotted_names() {
        for descriptor in catalog() {
            assert!(
                descriptor.name.contains('.'),
                "expected dotted name: {}",
                descriptor.name
            );
            assert!(
                !descriptor.name.contains('/'),
                "slash names forbidden: {}",
                descriptor.name
            );
        }
    }

    #[test]
    fn catalog_descriptors_have_non_empty_descriptions() {
        for descriptor in catalog() {
            assert!(
                !descriptor.description.is_empty(),
                "empty description for {}",
                descriptor.name
            );
        }
    }

    #[test]
    fn catalog_descriptors_have_params_schema_object() {
        for descriptor in catalog() {
            let schema = (descriptor.params_schema)();
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "params schema for {} must be an object",
                descriptor.name
            );
        }
    }

    #[test]
    fn catalog_impact_and_capability_match_method_constants() {
        let catalog = catalog();
        let checks: [(&MethodDescriptor, ImpactKind, RequiredCapability); 10] = [
            (&catalog[0], ReadFile::IMPACT, ReadFile::CAPABILITY),
            (&catalog[1], Grep::IMPACT, Grep::CAPABILITY),
            (&catalog[2], ListDirectory::IMPACT, ListDirectory::CAPABILITY),
            (&catalog[3], FindFiles::IMPACT, FindFiles::CAPABILITY),
            (&catalog[4], Fetch::IMPACT, Fetch::CAPABILITY),
            (&catalog[5], TodoWrite::IMPACT, TodoWrite::CAPABILITY),
            (&catalog[6], Run::IMPACT, Run::CAPABILITY),
            (&catalog[7], WriteFile::IMPACT, WriteFile::CAPABILITY),
            (&catalog[8], Patch::IMPACT, Patch::CAPABILITY),
            (&catalog[9], Declare::IMPACT, Declare::CAPABILITY),
        ];

        for (descriptor, impact, capability) in checks {
            assert_eq!(descriptor.impact, impact, "impact mismatch for {}", descriptor.name);
            assert_eq!(
                descriptor.capability, capability,
                "capability mismatch for {}",
                descriptor.name
            );
        }
    }

    #[test]
    fn catalog_len_is_ten() {
        assert_eq!(catalog().len(), 10);
    }
}
