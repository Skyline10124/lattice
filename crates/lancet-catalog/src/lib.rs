// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP typed method catalog.
//!
//! This crate owns the typed method contracts, params/results, method
//! descriptors, static production catalog, and schema data needed to project
//! those methods into other surfaces.

pub mod catalog;
pub mod method;
pub mod methods;
/// A synthetic typed method for tests. Behind `test-method` so it cannot be
/// linked into a production binary.
#[cfg(any(test, feature = "test-method"))]
pub mod test_method;

pub use catalog::{CATALOG, CATALOG_LEN, catalog};
#[cfg(feature = "inspect-scaffold")]
pub use catalog::{SCAFFOLD_CATALOG, scaffold_catalog};
pub use lancet_protocol::RequiredCapability;
pub use method::{BaseAuthorityRequirement, CatalogMethod, CepMethod, MethodDescriptor};
#[cfg(any(test, feature = "test-method"))]
pub use test_method::{TestEcho, TestEchoItem, TestEchoParams, TestEchoResult, TestEchoStatus};
