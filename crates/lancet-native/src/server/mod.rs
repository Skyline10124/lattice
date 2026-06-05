// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP server lifecycle, registry binding, and durable call metadata.

pub(crate) mod call_durable_metadata;
pub(crate) mod native_registry;
pub(crate) mod registry;
pub(crate) mod transport;

pub use native_registry::register_production_method_registry;
pub use registry::{HandlerContext, MethodRegistry};
pub use transport::{CepServerNativeExt, ServerRunConfig};
