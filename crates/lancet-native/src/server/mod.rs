// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP server lifecycle and registry binding.
pub(crate) mod evidence_respond;
pub(crate) mod native_registry;
pub(crate) mod registry;
pub(crate) mod transport;

pub use native_registry::register_production_method_registry;
pub use registry::{HandlerContext, MethodRegistry};
pub use transport::{CepServerNativeExt, ServerRunConfig};
