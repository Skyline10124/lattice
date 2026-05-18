#[cfg(test)]
mod tests {
    #[test]
    fn api_forbidden_dependency_imports_absent_from_source() {
        use std::fs;
        use std::path::Path;

        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden_prefixes = [
            "use removed_runtime_crate",
            "use removed_prompt_crate",
            "use removed_sandbox_crate",
            "use removed_policy_crate",
            "pub use removed_runtime_crate",
            "pub use removed_prompt_crate",
            "pub use removed_sandbox_crate",
            "pub use removed_policy_crate",
        ];

        fn check_dir(dir: &Path, forbidden: &[&str]) {
            for entry in fs::read_dir(dir).expect("should read dir") {
                let entry = entry.expect("should read entry");
                let path = entry.path();
                if path.is_dir() {
                    check_dir(&path, forbidden);
                } else if path.extension().map_or(true, |e| e != "rs") {
                    continue;
                } else {
                    let content = fs::read_to_string(&path).expect("should read file");
                    for (i, line) in content.lines().enumerate() {
                        let stripped = line.trim();
                        if stripped.starts_with("//")
                            || stripped.starts_with("//!")
                            || stripped.starts_with("///")
                        {
                            continue;
                        }
                        for pattern in forbidden {
                            if stripped.starts_with(pattern) {
                                panic!(
                                    "forbidden import in {}:{}: {}",
                                    path.display(),
                                    i + 1,
                                    line
                                );
                            }
                        }
                    }
                }
            }
        }

        check_dir(&src_dir, &forbidden_prefixes);
    }
}
