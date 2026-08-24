// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP server lifecycle layered on dispatch transport.

#[path = "handle.rs"]
mod handle;
#[path = "hosts.rs"]
mod hosts;
#[path = "runtime_ext.rs"]
mod runtime_ext;
#[path = "transport_config.rs"]
mod transport_config;

pub use runtime_ext::CepServerNativeExt;
pub use transport_config::ServerRunConfig;
