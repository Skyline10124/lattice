// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Inline CLI chat for LATTICE — REPL over stdin/stdout, no alternate screen.

mod app;
mod runtime;

pub use app::{AppConfig, AppEvent};
pub use runtime::{run, TuiOptions};
