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
pub use lattice_cep_protocol::{CapabilityPatterns, CapabilityResolveError, RequiredCapability};
pub use method::{CatalogMethod, CepMethod, MethodDescriptor};
