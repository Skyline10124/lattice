// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::io;
use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;

/// Initialize the tracing subscriber.
///
/// - `verbose=false`: error level only, compact format (module: message).
///   Default level is ERROR unless `LATTICE_LOG` overrides it.
/// - `verbose=true`: debug/trace output with timestamps, file+line, ANSI colors.
///   Equivalent to `LATTICE_LOG=debug`.
/// - `LATTICE_LOG` env var always takes priority over the `verbose` flag.
///   Supports the same syntax as `RUST_LOG` (e.g. `trace`, `lattice_llm=debug`).
///
/// Diagnostics go to stderr. `tracing_subscriber::fmt()` defaults to stdout,
/// which is where `--json` writes its JSONL: the non-verbose default level is
/// ERROR rather than off, so one provider error was enough to put a
/// non-JSON line in a stream the CLI documents as machine-readable (#100).
///
/// Returns `Err` if a subscriber was already set (i.e. `init_logging`
/// was called earlier). This is not fatal — logging simply continues
/// using the first subscriber.
pub fn init_logging(verbose: bool) -> Result<(), io::Error> {
    let default_level = if verbose { "debug" } else { "error" };

    let filter =
        EnvFilter::try_from_env("LATTICE_LOG").unwrap_or_else(|_| EnvFilter::new(default_level));

    let ansi = ansi_enabled(
        io::stderr().is_terminal(),
        std::env::var_os("NO_COLOR").is_some(),
    );

    if verbose {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(io::stderr)
            .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
            .with_file(true)
            .with_line_number(true)
            .with_ansi(ansi)
            .try_init()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "tracing subscriber already initialized",
                )
            })
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(io::stderr)
            .compact()
            .with_ansi(ansi)
            .try_init()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "tracing subscriber already initialized",
                )
            })
    }
}

/// Colour only an interactive stderr, and never against `NO_COLOR`.
///
/// Previously hardcoded per branch, so `--verbose` wrote escape sequences into
/// redirected output whatever the destination was.
fn ansi_enabled(stderr_is_terminal: bool, no_color_set: bool) -> bool {
    stderr_is_terminal && !no_color_set
}
