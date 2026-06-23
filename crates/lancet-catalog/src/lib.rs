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

pub use catalog::{CATALOG, catalog};
#[cfg(feature = "inspect-scaffold")]
pub use catalog::{SCAFFOLD_CATALOG, scaffold_catalog};
pub use lancet_protocol::RequiredCapability;
pub use method::{BaseAuthorityRequirement, CatalogMethod, CepMethod, MethodDescriptor};
