use std::io;
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
/// Returns `Err` if a subscriber was already set (i.e. `init_logging`
/// was called earlier). This is not fatal — logging simply continues
/// using the first subscriber.
pub fn init_logging(verbose: bool) -> Result<(), io::Error> {
    let default_level = if verbose { "debug" } else { "error" };

    let filter =
        EnvFilter::try_from_env("LATTICE_LOG").unwrap_or_else(|_| EnvFilter::new(default_level));

    if verbose {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
            .with_file(true)
            .with_line_number(true)
            .with_ansi(true)
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
            .compact()
            .with_ansi(false)
            .try_init()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "tracing subscriber already initialized",
                )
            })
    }
}
