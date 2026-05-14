/// Apply resource limits in the child process (Linux).
///
/// - RLIMIT_AS: 512 MB virtual memory
/// - RLIMIT_NPROC: 1 process
/// - RLIMIT_CPU: 30 seconds
///
/// Failures are silently ignored (logged at trace level).
#[cfg(target_os = "linux")]
pub fn apply_resource_limits_linux() {
    let rlim_as = libc::rlimit {
        rlim_cur: 512 * 1024 * 1024,
        rlim_max: 512 * 1024 * 1024,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values (non-negative
    // u64 constants). The return value is checked; a non-zero return is logged
    // at warn level but does not panic. This call is safe because: (1) we pass
    // stack-allocated structs with valid fields, (2) the kernel validates the
    // values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &rlim_as) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_AS) failed in child process");
    }

    let rlim_nproc = libc::rlimit {
        rlim_cur: 1,
        rlim_max: 1,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values (non-negative
    // u64 constants). The return value is checked; a non-zero return is logged
    // at warn level but does not panic. This call is safe because: (1) we pass
    // stack-allocated structs with valid fields, (2) the kernel validates the
    // values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &rlim_nproc) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_NPROC) failed in child process");
    }

    let rlim_cpu = libc::rlimit {
        rlim_cur: 30,
        rlim_max: 30,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values (non-negative
    // u64 constants). The return value is checked; a non-zero return is logged
    // at warn level but does not panic. This call is safe because: (1) we pass
    // stack-allocated structs with valid fields, (2) the kernel validates the
    // values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &rlim_cpu) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_CPU) failed in child process");
    }
}

/// Apply resource limits in the child process (macOS / other Unix).
///
/// - RLIMIT_AS: 512 MB virtual memory
/// - RLIMIT_NPROC: 32 (macOS limits entire process tree, not individual)
/// - RLIMIT_CPU: 30 seconds
///
/// Failures are silently ignored (logged at trace level).
#[cfg(all(unix, not(target_os = "linux")))]
pub fn apply_resource_limits_unix() {
    let rlim_as = libc::rlimit {
        rlim_cur: 512 * 1024 * 1024,
        rlim_max: 512 * 1024 * 1024,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values (non-negative
    // u64 constants). The return value is checked; a non-zero return is logged
    // at warn level but does not panic. This call is safe because: (1) we pass
    // stack-allocated structs with valid fields, (2) the kernel validates the
    // values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &rlim_as) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_AS) failed in child process");
    }

    let rlim_nproc = libc::rlimit {
        rlim_cur: 32,
        rlim_max: 32,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values (non-negative
    // u64 constants). The return value is checked; a non-zero return is logged
    // at warn level but does not panic. This call is safe because: (1) we pass
    // stack-allocated structs with valid fields, (2) the kernel validates the
    // values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &rlim_nproc) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_NPROC) failed in child process");
    }

    let rlim_cpu = libc::rlimit {
        rlim_cur: 30,
        rlim_max: 30,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values (non-negative
    // u64 constants). The return value is checked; a non-zero return is logged
    // at warn level but does not panic. This call is safe because: (1) we pass
    // stack-allocated structs with valid fields, (2) the kernel validates the
    // values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &rlim_cpu) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_CPU) failed in child process");
    }
}
