// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Dedicated git2 migration writer (D-05, D-06, D-07, D-09, D-10).
//!
//! Filled in by Plan 39-02. Declared here so `migrate/mod.rs` compiles after
//! Plan 02 lands. The writer creates Git commit objects on arbitrary parents
//! (bypassing the CAS-enforcing `commit_prepared_turn` path), writes session
//! and candidate refs idempotently, persists sidecars (`meta.json`, `ops/`),
//! and checkpoints the idmap.
