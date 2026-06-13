// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP dispatch and in-process transport.

pub mod registry;
pub mod transport;

pub use registry::{
    Cancellation, EventSink, MethodRegistry, call_cancelled, duplicate_registration,
    wire_code_for_error,
};
pub use transport::{CallControl, CepClient, CepServer, CepTransport, ServerCall, ServerRuntime};
