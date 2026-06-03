//! Effect-region contract tests.
//!
//! Native handler runtime effects are checked against catalog-declared effect
//! kinds and authority regions. Shared fixtures stay local to this module tree
//! so tests can still use crate-private native handler context.

mod fixtures;
mod properties;
mod regression;
mod runtime_fixtures;
mod runtime_matrix;
mod support;
