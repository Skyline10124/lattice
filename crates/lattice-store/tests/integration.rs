// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `$XDG_STATE_HOME` 状态根解析与回退集成测试。

use std::path::Path;

use lattice_store::{StoreError, xdg_state_root};

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
