// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the acceptance-critical kernel behaviors that need
//! real directories and processes (GitHub #143):
//!
//! - raw path-byte `ProjectStoreKey` coverage: same-path reuse, move ⇒ new
//!   key, non-UTF8 paths, symlink canonical identity;
//! - `$XDG_STATE_HOME` fallback semantics;
//! - SIGKILL/reopen consistency of first-create: a store is either absent or
//!   fully valid, never half-published (ADR 0005 §18: this demonstrates
//!   process-crash consistency, not hardware power-loss consistency).

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use lattice_store::{
    ProjectStoreKey, StateLayout, StoreError, StoreMeta, StoreRepo, StoreScope, xdg_state_root,
};

fn meta() -> StoreMeta {
    StoreMeta {
        store_kind: "session".into(),
        store_schema_version: 1,
        scope: StoreScope::Global,
    }
}

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

#[test]
fn create_returns_after_releasing_create_lock_and_repo_lock_is_distinct() {
    let state = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let repo = StoreRepo::create_global(&layout, "sessions.git", meta(), |_| Ok(())).unwrap();

    // The first-create lock file remains in the parent directory but is released
    // before create returns. Steady-state repository writes use a distinct lock
    // inside the repository.
    let create_lock = layout.lattice_root().join(".sessions.git.create.lock");
    assert!(create_lock.exists());

    let write = repo.write_lock().expect("repository write lock");
    assert!(write.path().starts_with(repo.git_dir()));
    assert!(write.path().ends_with("lattice-repo.lock"));
    assert_ne!(write.path(), create_lock.as_path());
    drop(write);

    let create = lattice_store::lock::acquire_flock_strict(
        &create_lock,
        lattice_store::Blocking::NonBlocking,
    )
    .expect("create lock must be free after create returns");
    drop(create);
}

/// Child mode: publish one store, then stop inside a second store's init callback
/// so the parent can SIGKILL us at a known pre-publication point.
fn crash_child_loop(dir: &Path) {
    let layout = StateLayout::in_state_root(dir);
    StoreRepo::create_global(&layout, "store-0.git", meta(), |_| Ok(())).unwrap();
    StoreRepo::create_global(&layout, "store-1.git", meta(), |_| {
        std::fs::write(dir.join("crash-child-ready"), b"ready").unwrap();
        loop {
            std::thread::park();
        }
    })
    .unwrap();
}

#[test]
fn sigkill_mid_create_leaves_only_absent_or_fully_valid_stores() {
    if let Ok(dir) = std::env::var("LATTICE_STORE_CRASH_CHILD") {
        crash_child_loop(Path::new(&dir));
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut child = std::process::Command::new(exe)
        .args([
            "sigkill_mid_create_leaves_only_absent_or_fully_valid_stores",
            "--exact",
            "--nocapture",
        ])
        .env("LATTICE_STORE_CRASH_CHILD", dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn crash child");

    // Wait until the child has published one complete store and is blocked
    // inside a second store's pre-publication init callback, then SIGKILL it.
    let ready = dir.path().join("crash-child-ready");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let kill_result = loop {
        if ready.exists() {
            break unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
        }
        if let Some(status) = child.try_wait().expect("poll crash child") {
            panic!("crash child exited before reaching the kill point: {status}");
        }
        if std::time::Instant::now() >= deadline {
            unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
            child.wait().expect("reap timed-out crash child");
            panic!("crash child did not reach the kill point");
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert_eq!(kill_result, 0, "SIGKILL must reach the crash child");
    let status = child.wait().expect("wait for child");
    assert!(!status.success(), "child should have been killed");

    // Every target is either absent or a fully valid store; a stale sibling
    // temp never blocks recreation.
    let layout = StateLayout::in_state_root(dir.path());
    StoreRepo::open_global(&layout, "store-0.git", "session", 1).unwrap();
    assert!(
        !layout.lattice_root().join("store-1.git").exists(),
        "the store interrupted before publication must remain absent"
    );

    // A fresh create at the same state root still works (stale temps and
    // lock files from the dead child are reclaimed/harmless).
    StoreRepo::create_global(&layout, "after-crash.git", meta(), |_| Ok(())).unwrap();
    StoreRepo::open_global(&layout, "after-crash.git", "session", 1).unwrap();

    // And re-creating the target interrupted before publication works too.
    StoreRepo::create_global(&layout, "store-1.git", meta(), |_| Ok(())).unwrap();
    StoreRepo::open_global(&layout, "store-1.git", "session", 1).unwrap();
}
