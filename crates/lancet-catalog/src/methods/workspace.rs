// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Workspace mutation catalog types.
//!
//! `mutate.create` and `mutate.edit` used to live here as `CepMethod` /
//! `CatalogMethod` types. #31 migrated both to embedded Component Tools, so
//! their method types, params/result pairs, and `CepMethod`/`CatalogMethod`
//! impls are gone: `tools/mutate-create/manifest.toml` and
//! `tools/mutate-edit/manifest.toml` are now the sole descriptor and schema
//! authorities for those methods. Re-adding any of them here would be the
//! parallel schema ADR 0004 forbids, and a name in both vocabularies fails
//! the server run closed (`validate_no_component_native_collision`).
//!
//! `observe.workspace` was already migrated the same way in #30
//! (`tools/observe-workspace/manifest.toml`).
