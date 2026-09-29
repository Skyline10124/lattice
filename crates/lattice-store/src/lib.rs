// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-store`: the domain-neutral private-Git/Linux durability kernel
//! (ADR 0005 §4).
//!
//! The kernel owns only reusable mechanics:
//!
//! - [`ProjectStoreKey`] derivation and the XDG state-root layout
//!   ([`StateLayout`]);
//! - typed create/open with repository identity validation
//!   ([`StoreRepo`], [`StoreMeta`], `refs/lattice/meta`);
//! - canonical bytes ([`canon`]) and immutable object/commit creation;
//! - 单个仓库内的引用 compare-and-set；
//! - per-repository Linux `flock` ([`StoreRepo::write_lock`]) and the shared
//!   [`acquire_flock`] primitive the domain operation locks build on;
//! - file, ref, and directory durability barriers ([`durability`]);
//! - atomic first-create publication (`renameat2(RENAME_NOREPLACE)`);
//! - capability probes per actual publication location ([`probe`]);
//! - typed corruption/durability errors ([`StoreError`]).
//!
//! It does **not** own a catalog of domain schemas, a generic
//! `put(key, value)` API, a raw `gix::Repository` getter, an all-stores
//! handle, a pluggable backend, or cross-store transactions. Domain wrappers
//! own their repository names, refs, record schemas, invariants, and
//! migrations. Layer 0: no workspace-internal dependencies.

pub mod canon;
pub mod durability;
mod error;
pub mod fault;
pub mod key;
pub mod layout;
pub mod lock;
pub mod meta;
pub mod probe;
pub mod repo;

pub use error::StoreError;
pub use key::ProjectStoreKey;
pub use layout::{LATTICE_STATE_DIR, StateLayout, xdg_state_root};
pub use lock::{Blocking, FlockGuard, LockMode, acquire_flock};
pub use meta::{ExpectedMeta, META_REF, REPOSITORY_FORMAT_VERSION, StoreMeta, StoreScope};
pub use repo::{LATTICE_REF_PREFIX, REPO_LOCK_FILE, RefExpectation, StoreRepo, TreeSpec};
