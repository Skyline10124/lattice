use std::io;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

/// Initialize the tracing subscriber.
///
/// - `verbose=false`: error level only, compact format (module: message).
///   Default level is ERROR unless `ARTEMIS_LOG` overrides it.
/// - `verbose=true`: debug/trace output with timestamps, file+line, ANSI colors.
///   Equivalent to `ARTEMIS_LOG=debug`.
/// - `ARTEMIS_LOG` env var always takes priority over the `verbose` flag.
///   Supports the same syntax as `RUST_LOG` (e.g. `trace`, `artemis_core=debug`).
///
/// Returns `Err` if a subscriber was already set (i.e. `init_logging` or
/// `init_debug_logging` was called earlier). This is not fatal — logging
/// simply continues using the first subscriber.
pub fn init_logging(verbose: bool) -> Result<(), io::Error> {
    let default_level = if verbose { "debug" } else { "error" };

    let filter =
        EnvFilter::try_from_env("ARTEMIS_LOG").unwrap_or_else(|_| EnvFilter::new(default_level));

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

/// Initialize trace-level logging for the `debug` subcommand.
///
/// Forces trace-level and uses colored, detailed output on console.
/// Also writes to `log_path` with timestamps and levels.
///
/// Returns `Err` if the log file cannot be opened or if a subscriber
/// was already set.
pub fn init_debug_logging(log_path: &str) -> Result<(), io::Error> {
    use std::fs;

    if let Some(parent) = std::path::Path::new(log_path).parent() {
        fs::create_dir_all(parent)?;
    }

    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    let file_filter = EnvFilter::new("trace");

    // Console: colored, detailed, trace level
    let console_layer = tracing_subscriber::fmt::layer()
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .with_file(true)
        .with_line_number(true)
        .with_ansi(true)
        .with_filter(EnvFilter::new("trace"));

    // File: plain, timestamps + level, trace level
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file)
        .with_ansi(false)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .with_filter(file_filter);

    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .try_init()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "tracing subscriber already initialized",
            )
        })
}
