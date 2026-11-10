// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the acceptance-critical kernel behaviors that need
//! real directories and processes (GitHub #143):
//!
//! - raw path-byte `ProjectStoreKey` coverage: same-path reuse, move ⇒ new
//!   key, non-UTF8 paths, symlink canonical identity;
//! - `$XDG_STATE_HOME` fallback semantics.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use lattice_store::{ProjectStoreKey, StoreError, xdg_state_root};

#[test]
fn project_store_key_path_semantics() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    std::fs::create_dir(&project).unwrap();

    // Same canonical path ⇒ same key (same-path reuse).
    let key_a = ProjectStoreKey::derive(&project).unwrap();
    let key_b = ProjectStoreKey::derive(&project).unwrap();
    assert_eq!(key_a, key_b);

    // Symlinked spelling canonicalizes to the same key.
    let link = root.path().join("proj-link");
    std::os::unix::fs::symlink(&project, &link).unwrap();
    assert_eq!(ProjectStoreKey::derive(&link).unwrap(), key_a);

    // Moving the project ⇒ new key.
    let moved = root.path().join("moved");
    std::fs::rename(&project, &moved).unwrap();
    let key_moved = ProjectStoreKey::derive(&moved).unwrap();
    assert_ne!(key_a, key_moved);

    // An unrelated project reusing the old path sees the old namespace.
    std::fs::create_dir(&project).unwrap();
    assert_eq!(ProjectStoreKey::derive(&project).unwrap(), key_a);
}

#[test]
fn project_store_key_supports_non_utf8_paths() {
    let root = tempfile::tempdir().unwrap();
    // A directory whose name is not valid UTF-8.
    let weird = root.path().join("weird");
    std::fs::create_dir(&weird).unwrap();
    let non_utf8 = {
        use std::ffi::OsStr;
        let mut bytes = weird.as_os_str().as_bytes().to_vec();
        bytes.extend_from_slice(b"/\xff\xfe\xfd");
        std::path::PathBuf::from(OsStr::from_bytes(&bytes))
    };
    std::fs::create_dir(&non_utf8).unwrap();
    let key = ProjectStoreKey::derive(&non_utf8).unwrap();
    assert_eq!(key.to_string().len(), 64);
    // And it is stable.
    assert_eq!(ProjectStoreKey::derive(&non_utf8).unwrap(), key);
}

#[test]
fn xdg_state_root_fallback_order() {
    // This crate's tests never read the process env except here.
    let prev_xdg = std::env::var_os("XDG_STATE_HOME");
    let prev_home = std::env::var_os("HOME");

    // 1. Absolute XDG_STATE_HOME wins.
    unsafe { std::env::set_var("XDG_STATE_HOME", "/tmp/xdg-absolute") };
    assert_eq!(xdg_state_root().unwrap(), Path::new("/tmp/xdg-absolute"));

    // 2. A relative value is ignored per the XDG spec; $HOME fallback applies.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", "relative/state");
        std::env::set_var("HOME", "/tmp/home-fallback");
    }
    assert_eq!(
        xdg_state_root().unwrap(),
        Path::new("/tmp/home-fallback/.local/state")
    );

    // 3. Neither available ⇒ typed error.
    unsafe {
        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("HOME");
    }
    assert!(matches!(
        xdg_state_root().unwrap_err(),
        StoreError::MissingStateRoot
    ));

    unsafe {
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
