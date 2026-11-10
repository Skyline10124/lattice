// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-store`：Linux 私有状态与持久性基础原语。
//!
//! 本 crate 负责：
//!
//! - XDG 状态根布局（[`StateLayout`]、[`xdg_state_root`]）与 [`ProjectStoreKey`] 推导；
//! - Linux `flock` 操作锁（[`acquire_flock`]）与同进程票号 FIFO 严格锁（[`lock::acquire_flock_strict`]）；
//! - 文件与目录 [`durability::fsync_file`]、[`durability::fsync_dir`] 同步屏障；
//! - 按实际发布位置的文件系统能力探测（[`probe`]）；
//! - 类型化存储与持久性错误（[`StoreError`]）。
//!
//! 零工作区内部依赖。

pub mod durability;
mod error;
pub mod key;
pub mod layout;
pub mod lock;
pub mod probe;

pub use error::StoreError;
pub use key::ProjectStoreKey;
pub use layout::{LATTICE_STATE_DIR, StateLayout, xdg_state_root};
pub use lock::{Blocking, FlockGuard, LockMode, acquire_flock};
