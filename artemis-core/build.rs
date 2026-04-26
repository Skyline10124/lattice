fn main() {
    // Link against libpython for test binaries.
    // The pyo3 `extension-module` feature (enabled by default for maturin builds)
    // prevents automatic linking against libpython because symbols are resolved
    // at runtime by the Python interpreter. However, test executables need these
    // symbols resolved at link time.
    let Ok(output) = std::process::Command::new("python3-config")
        .arg("--ldflags")
        .arg("--embed")
        .output()
    else {
        return;
    };
    let flags = String::from_utf8_lossy(&output.stdout);
    for flag in flags.split_whitespace() {
        if let Some(path) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search={}", path);
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={}", lib);
        }
    }
}
