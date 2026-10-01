// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsString;
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use tempfile::TempDir;

static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// 唯一的测试 XDG guard；下游只通过 dev-dependency 启用 test-support。
/// 参与者须遵守同一锁，不能与未受控环境读写并发使用。
pub struct ScopedXdgStateHome {
    _lock: MutexGuard<'static, ()>,
    state: TempDir,
    prev_xdg: Option<OsString>,
}

impl ScopedXdgStateHome {
    pub fn new() -> Self {
        let guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let prev_xdg = std::env::var_os("XDG_STATE_HOME");
        let state = tempfile::tempdir().expect("xdg state tempdir");
        // SAFETY: 测试调用方须保证环境访问隔离，且所有参与者使用同一锁。
        unsafe {
            std::env::set_var("XDG_STATE_HOME", state.path());
        }
        Self {
            _lock: guard,
            state,
            prev_xdg,
        }
    }

    pub fn path(&self) -> &Path {
        self.state.path()
    }
}

impl Default for ScopedXdgStateHome {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedXdgStateHome {
    fn drop(&mut self) {
        // SAFETY: 恢复发生在字段 guard 释放前，前提同 new。
        unsafe {
            match &self.prev_xdg {
                Some(value) => std::env::set_var("XDG_STATE_HOME", value),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }
}
