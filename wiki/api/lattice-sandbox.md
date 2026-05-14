# lattice-sandbox API

`lattice-sandbox` owns tool execution policy that is independent of agent logic.

## Main Types

- `SandboxConfig`: read/write allowlists, command allowlist, hook, Landlock, audit and size limits.
- `LandlockConfig`: Linux Landlock file isolation settings.
- `CommandHook`: optional command policy hook interface.
- `AuditSink`: sink trait for sandbox audit events.
- `SandboxError`: structured sandbox error type.

## Checks

`SandboxConfig` exposes:

- `check_read(path)`
- `check_write(path)`
- `check_command(command)`
- `check_url(url)`

Command checks reject shell metacharacter injection, environment override attacks and commands outside the allowlist. URL checks reject unsupported schemes and private/reserved HTTPS hosts.

## Landlock

Landlock is Linux-specific. Non-Linux platforms still use command, path and URL checks but cannot apply kernel file isolation.
