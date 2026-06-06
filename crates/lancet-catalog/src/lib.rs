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

pub use catalog::{catalog, CATALOG};
#[cfg(feature = "inspect-scaffold")]
pub use catalog::{scaffold_catalog, SCAFFOLD_CATALOG};
pub use lancet_protocol::{CapabilityPatterns, CapabilityResolveError, RequiredCapability};
pub use method::{BaseAuthorityRequirement, CatalogMethod, CepMethod, MethodDescriptor};
