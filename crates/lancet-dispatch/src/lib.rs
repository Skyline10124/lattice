// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP dispatch and in-process transport.

pub mod registry;
pub mod transport;

pub use registry::{
    call_cancelled, duplicate_registration, wire_code_for_error, Cancellation, EventSink,
    MethodRegistry,
};
pub use transport::{CallControl, CepClient, CepServer, CepTransport, ServerCall, ServerRuntime};
