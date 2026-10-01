// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Trust boundary for the workspace `.env` file.
//!
//! The CLI still loads a `.env` file discovered from the current directory
//! upwards, because putting provider API keys there is the file's legitimate
//! use. But the working directory belongs to whatever repository the user
//! happens to be standing in, so a `.env` is *untrusted input*: cloning a
//! third-party repository and running `lattice` inside it must not let that
//! repository change where LATTICE reads its policy from, or where it sends
//! conversation traffic.
//!
//! So keys are partitioned:
//!
//! * Ordinary keys (provider credentials, model names, anything the product
//!   does not use to locate policy or route traffic) are injected exactly as
//!   before — and, as before, never override a value already present in the
//!   process environment.
//! * Keys in [`RejectReason`]'s categories are dropped unconditionally, with
//!   the drop recorded so `lattice doctor` can show it. Dropping is silent at
//!   startup on purpose: a hostile `.env` must not get to print anything, and a
//!   benign one (a developer who exported `HTTPS_PROXY` there) must not turn
//!   every command into a warning.
//!
//! The process environment is unaffected by this module's rules: an operator
//! who exports `LATTICE_AGENTS_DIR` in their shell, a CI job, or a systemd unit
//! is speaking with the user's own authority, not the checked-out repository's.
//!
//! Loading happens once, before the tokio runtime is built, because
//! `std::env::set_var` is only sound while the process is single-threaded.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Why a `.env` key was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// `LATTICE_*` — the product's own runtime switches. `LATTICE_AGENTS_DIR`
    /// selects the agent profile directory, i.e. the system prompt, the exposed
    /// tool set, and every `[lancet]` policy section.
    LatticeNamespace,
    /// A provider endpoint override such as `OPENAI_COMPATIBLE_BASE_URL`.
    /// net-guard only rejects private/reserved addresses, so a public attacker
    /// gateway would be dialled normally, with conversation text and the API
    /// key attached.
    ProviderEndpoint,
    /// `*_PROXY`. `reqwest` picks system proxies up from the environment by
    /// default, so these reroute provider traffic just as effectively as a
    /// base-URL override does.
    Proxy,
    /// `HOME` / `XDG_*`. These resolve the user-global agents directory, the
    /// session store, and the audit / private-history state root.
    StateRoot,
    /// `SSL_CERT_FILE` / `SSL_CERT_DIR`. On Linux the TLS root store comes from
    /// `rustls-native-certs` (through `reqwest`'s platform verifier), which
    /// honours both — so a repository could install its own trust anchor.
    TlsTrust,
}

impl RejectReason {
    /// Stable machine-readable tag for `--json` output.
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::LatticeNamespace => "lattice_namespace",
            RejectReason::ProviderEndpoint => "provider_endpoint",
            RejectReason::Proxy => "proxy",
            RejectReason::StateRoot => "state_root",
            RejectReason::TlsTrust => "tls_trust",
        }
    }

    /// Short human explanation shown by `lattice doctor`.
    pub fn describe(self) -> &'static str {
        match self {
            RejectReason::LatticeNamespace => "LATTICE runtime switch",
            RejectReason::ProviderEndpoint => "provider endpoint override",
            RejectReason::Proxy => "proxy override",
            RejectReason::StateRoot => "home/state root override",
            RejectReason::TlsTrust => "TLS trust anchor override",
        }
    }
}

/// A `.env` key that was read but deliberately not applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IgnoredKey {
    pub key: String,
    pub reason: RejectReason,
}

/// Where an environment value in effect right now came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueSource {
    /// The variable is not set in this process.
    Unset,
    /// Inherited from the parent process environment.
    Process,
    /// Injected from the workspace `.env`.
    Dotenv,
}

impl ValueSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ValueSource::Unset => "unset",
            ValueSource::Process => "process",
            ValueSource::Dotenv => "dotenv",
        }
    }
}

/// What the workspace `.env` load actually did.
#[derive(Clone, Debug, Default)]
pub struct DotenvReport {
    /// The `.env` that was found, if any.
    pub path: Option<PathBuf>,
    /// Keys injected into the process environment, sorted.
    pub applied: Vec<String>,
    /// Keys refused by the trust boundary, sorted.
    pub ignored: Vec<IgnoredKey>,
    /// Keys the process environment already defined, so the `.env` value lost.
    pub shadowed: Vec<String>,
    /// Read/parse failure for a `.env` that exists but could not be used.
    pub error: Option<String>,
}

impl DotenvReport {
    /// Resolve where the value currently visible under `key` came from.
    pub fn source_of(&self, key: &str) -> ValueSource {
        if std::env::var_os(key).is_none() {
            return ValueSource::Unset;
        }
        if self.applied.iter().any(|applied| applied == key) {
            ValueSource::Dotenv
        } else {
            ValueSource::Process
        }
    }

    /// Render the `.env` path for display, or `None` when no file was found.
    pub fn path_display(&self) -> Option<String> {
        self.path.as_ref().map(|p| p.display().to_string())
    }
}

static REPORT: OnceLock<DotenvReport> = OnceLock::new();

/// Load the workspace `.env` under the trust boundary. Idempotent: the first
/// call performs the load, later calls return the same report.
///
/// Must be called while the process is still single-threaded.
pub fn load_workspace_dotenv() -> &'static DotenvReport {
    REPORT.get_or_init(|| {
        let Some(path) = find_dotenv() else {
            return DotenvReport::default();
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return DotenvReport {
                    path: Some(path),
                    error: Some(error.to_string()),
                    ..DotenvReport::default()
                };
            }
        };

        let plan = plan_dotenv(&path, &bytes, |key| std::env::var_os(key).is_some());
        for (key, value) in &plan.to_apply {
            // SAFETY: called from `main` before the tokio runtime is built, so
            // the process is single-threaded and no other thread can be
            // reading the environment concurrently.
            unsafe { std::env::set_var(key, value) };
        }
        plan.report
    })
}

/// 仅 main 在创建运行时之前调用；后台不继承提交端的凭据、策略或 TLS 环境。
pub fn prepare_daemon_environment(state_root: &Path) {
    // 此时进程仍单线程；先收集键，避免迭代器与环境修改交错。
    let keys: Vec<_> = std::env::vars_os().map(|(key, _)| key).collect();
    // 仅显式屏障构建保留既有故障接口；后台启动后不接受前端环境切换。
    #[cfg(feature = "test-crash-barriers")]
    let injections: Vec<_> = [
        "LATTICE_TEST_TURN_BARRIER",
        "LATTICE_TEST_TURN_BARRIER_ACK",
        "LATTICE_TEST_SCHEDULE_DIR",
        "LATTICE_TEST_TURN_INJECTION",
        "LATTICE_TEST_TOOL_EFFECT_LOG",
        "LATTICE_TEST_REWIND_BARRIER",
        "LATTICE_TEST_REWIND_BARRIER_ACK",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (key, value)))
    .collect();
    unsafe {
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("LANG", "C.UTF-8");
        std::env::set_var("HOME", state_root.join("lattice/local"));
        std::env::set_var("XDG_CONFIG_HOME", state_root.join("lattice/local/config"));
        std::env::set_var("XDG_STATE_HOME", state_root);
        #[cfg(feature = "test-crash-barriers")]
        for (key, value) in injections {
            std::env::set_var(key, value);
        }
    }
}

/// A decided `.env` load: the report to publish plus the key/value pairs to
/// install. Split out from [`load_workspace_dotenv`] so every decision the
/// boundary makes is testable without mutating the test process's environment
/// — including the non-override rule, which is the mechanism behind "a `.env`
/// never beats the process environment" and would otherwise have no coverage.
struct DotenvPlan {
    report: DotenvReport,
    to_apply: Vec<(String, String)>,
}

/// Parse `.env` bytes and partition the keys into applied / shadowed / ignored.
///
/// `already_set` reports whether a key is already defined in the target
/// environment; it is injected so tests can drive precedence directly.
fn plan_dotenv(path: &Path, bytes: &[u8], already_set: impl Fn(&str) -> bool) -> DotenvPlan {
    let mut report = DotenvReport {
        path: Some(path.to_path_buf()),
        ..DotenvReport::default()
    };

    // Strip a UTF-8 BOM before parsing. `dotenvy`'s own loaders do this inside
    // the private `Iter::load`, which the manual iteration below bypasses, so
    // without this a `.env` saved by a BOM-writing editor would fail to parse
    // on line 0 and silently drop every key in the file.
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);

    let dependency_entries = match substitution_dependencies(body) {
        Ok(entries) => entries,
        Err(error) => {
            report.error = Some(error);
            return DotenvPlan {
                report,
                to_apply: Vec::new(),
            };
        }
    };
    let mut current_taint = BTreeMap::<String, bool>::new();
    let mut occurrence_taint = dependency_entries
        .into_iter()
        .map(|(key, refs)| {
            let tainted = classify(&key).is_some()
                || refs.iter().any(|referenced| {
                    classify(referenced).is_some()
                        || current_taint.get(referenced).copied().unwrap_or(false)
                });
            // dotenvy updates substitution state for every occurrence,
            // including duplicates that non-override loading will not apply.
            current_taint.insert(key.clone(), tainted);
            (key, tainted)
        })
        .collect::<Vec<_>>()
        .into_iter();

    // Keep the first occurrence of a repeated key, matching dotenvy's own
    // non-override `load()` semantics.
    let mut pending: BTreeMap<String, String> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for entry in dotenvy::from_read_iter(body) {
        match entry {
            Ok((key, value)) => {
                let Some((raw_key, tainted)) = occurrence_taint.next() else {
                    report.error = Some("workspace .env dependency scan diverged".to_string());
                    return DotenvPlan {
                        report,
                        to_apply: Vec::new(),
                    };
                };
                if raw_key != key {
                    report.error = Some("workspace .env dependency scan diverged".to_string());
                    return DotenvPlan {
                        report,
                        to_apply: Vec::new(),
                    };
                }
                if !seen.insert(key.clone()) {
                    continue;
                }
                if let Some(reason) = classify(&key) {
                    if !report.ignored.iter().any(|ignored| ignored.key == key) {
                        report.ignored.push(IgnoredKey { key, reason });
                    }
                    continue;
                }
                if tainted {
                    // `dotenvy` expands from both earlier assignments and the
                    // real process environment. Dependency taint is therefore
                    // derived from the raw assignment, never guessed from the
                    // expanded value. Never include either value in diagnostics.
                    report.error.get_or_insert_with(|| {
                        format!("unsafe .env substitution into {key} was refused")
                    });
                    continue;
                }
                pending.entry(key).or_insert(value);
            }
            Err(error) => {
                // dotenvy aborts iteration at the first malformed line, so keys
                // after it are dropped. That matches the previous
                // `dotenvy::dotenv()` behaviour; `report.error` makes it
                // visible in `lattice doctor` instead of silent.
                report.error = Some(safe_parse_error(&error));
                break;
            }
        }
    }

    let mut to_apply = Vec::new();
    for (key, value) in pending {
        if already_set(&key) {
            report.shadowed.push(key);
            continue;
        }
        report.applied.push(key.clone());
        to_apply.push((key, value));
    }

    report.ignored.sort_by(|a, b| a.key.cmp(&b.key));
    report.shadowed.sort();
    DotenvPlan { report, to_apply }
}

/// Raw substitution dependencies by assignment key.
///
/// `dotenvy` does not expose this provenance after expansion and consults the
/// real process environment before its internal assignment table. We inspect
/// only syntax here, then leave value parsing to `dotenvy`; this prevents a
/// rejected name from laundering either its file value or a process value into
/// an allowed assignment.
fn substitution_dependencies(body: &[u8]) -> Result<Vec<(String, BTreeSet<String>)>, String> {
    let text =
        std::str::from_utf8(body).map_err(|_| "workspace .env is not valid UTF-8".to_string())?;
    let mut dependencies = Vec::new();
    let mut entry = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut comment_eligible = true;

    for ch in text.chars() {
        if comment {
            if ch == '\n' {
                record_dependencies(&entry, &mut dependencies);
                entry.clear();
                comment = false;
                comment_eligible = true;
            }
            continue;
        }
        let comment_start = ch == '#' && quote.is_none() && !escaped && comment_eligible;
        if comment_start {
            comment = true;
            continue;
        }
        entry.push(ch);
        if escaped {
            escaped = false;
            comment_eligible = false;
            continue;
        }
        match (quote, ch) {
            (Some(active), current) if active == current => {
                quote = None;
                comment_eligible = false;
            }
            (Some('"'), '\\') => {
                escaped = true;
                comment_eligible = false;
            }
            (None, '\\') => {
                escaped = true;
                comment_eligible = false;
            }
            (None, '\'' | '"') => {
                quote = Some(ch);
                comment_eligible = false;
            }
            (None, '\n') => {
                record_dependencies(&entry, &mut dependencies);
                entry.clear();
                comment_eligible = true;
            }
            (None, ' ' | '\t') => comment_eligible = true,
            _ => comment_eligible = false,
        }
    }
    if !entry.is_empty() {
        record_dependencies(&entry, &mut dependencies);
    }
    Ok(dependencies)
}

fn record_dependencies(entry: &str, dependencies: &mut Vec<(String, BTreeSet<String>)>) {
    let Some((key, value)) = raw_assignment(entry) else {
        return;
    };
    dependencies.push((key, substitution_refs(value)));
}

/// Split a logical dotenv assignment without interpreting its value.
fn raw_assignment(entry: &str) -> Option<(String, &str)> {
    let mut rest = entry.trim_start();
    if rest.is_empty() || rest.starts_with('#') {
        return None;
    }

    let (mut key, after_key) = raw_key(rest)?;
    rest = after_key.trim_start();
    if key == "export" && !rest.starts_with('=') {
        let (exported, after_exported) = raw_key(rest)?;
        key = exported;
        rest = after_exported.trim_start();
    }
    rest.strip_prefix('=')
        .map(|value| (key, value.trim_start()))
}

fn raw_key(input: &str) -> Option<(String, &str)> {
    let end = input
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
        .unwrap_or(input.len());
    let key = &input[..end];
    if key.is_empty() || !key.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_') {
        return None;
    }
    Some((key.to_string(), &input[end..]))
}

fn substitution_refs(value: &str) -> BTreeSet<String> {
    let chars = value.char_indices().collect::<Vec<_>>();
    let mut refs = BTreeSet::new();
    let mut index = 0;
    let mut strong_quote = false;
    let mut weak_quote = false;
    let mut escaped = false;
    while index < chars.len() {
        let (_, ch) = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if ch == '\\' && !strong_quote {
            escaped = true;
            index += 1;
            continue;
        }
        if ch == '\'' && !weak_quote {
            strong_quote = !strong_quote;
            index += 1;
            continue;
        }
        if ch == '"' && !strong_quote {
            weak_quote = !weak_quote;
            index += 1;
            continue;
        }
        if ch != '$' || strong_quote {
            index += 1;
            continue;
        }

        index += 1;
        if index >= chars.len() {
            break;
        }
        if chars[index].1 == '{' {
            index += 1;
            let start = index;
            while index < chars.len() && chars[index].1 != '}' {
                index += 1;
            }
            if index < chars.len() {
                let name = chars[start..index]
                    .iter()
                    .map(|(_, ch)| *ch)
                    .collect::<String>();
                if !name.is_empty() {
                    refs.insert(name);
                }
            }
        } else {
            let start = index;
            while index < chars.len() && chars[index].1.is_alphanumeric() {
                index += 1;
            }
            let name = chars[start..index]
                .iter()
                .map(|(_, ch)| *ch)
                .collect::<String>();
            if !name.is_empty() {
                refs.insert(name);
            }
            continue;
        }
        index += 1;
    }
    refs
}

/// Render a dotenv parser failure without echoing the source line. A malformed
/// assignment can contain a credential, and `dotenvy`'s `Display` includes the
/// complete offending line for `LineParse`.
fn safe_parse_error(error: &dotenvy::Error) -> String {
    match error {
        dotenvy::Error::LineParse(_, offset) => {
            format!("invalid .env syntax at byte offset {offset}")
        }
        other => other.to_string(),
    }
}

/// The report from the startup load. Returns an empty report if the load never
/// ran (any binary path that skips `load_workspace_dotenv`).
pub fn report() -> &'static DotenvReport {
    static EMPTY: OnceLock<DotenvReport> = OnceLock::new();
    REPORT
        .get()
        .unwrap_or_else(|| EMPTY.get_or_init(DotenvReport::default))
}

/// Decide whether a `.env` key crosses the trust boundary.
///
/// Matching is case-insensitive: environment variable names are
/// case-sensitive on Linux, and some of these keys are genuinely read in
/// lowercase form (`https_proxy`), so a case-sensitive rule would leave a
/// bypass open.
pub fn classify(key: &str) -> Option<RejectReason> {
    let upper = key.trim().to_ascii_uppercase();

    if upper.starts_with("LATTICE_") {
        return Some(RejectReason::LatticeNamespace);
    }
    if upper.ends_with("_BASE_URL")
        || upper.ends_with("_API_BASE")
        || upper.ends_with("_API_URL")
        || upper.ends_with("_ENDPOINT")
    {
        return Some(RejectReason::ProviderEndpoint);
    }
    // Covers HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY and their
    // lowercase spellings.
    if upper.ends_with("_PROXY") {
        return Some(RejectReason::Proxy);
    }
    if upper == "HOME" || upper.starts_with("XDG_") {
        return Some(RejectReason::StateRoot);
    }
    // On Linux `reqwest`'s platform verifier loads roots through
    // `rustls-native-certs`, which *replaces* the platform store with these
    // paths when either is set — so this is a stronger MITM primitive than an
    // endpoint override, not a weaker one.
    if upper == "SSL_CERT_FILE" || upper == "SSL_CERT_DIR" {
        return Some(RejectReason::TlsTrust);
    }

    None
}

/// Search the current directory and its ancestors for a `.env`, matching the
/// discovery behaviour `dotenvy::dotenv()` had before this module existed. The
/// path is needed separately so `lattice doctor` can name the source file.
fn find_dotenv() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    find_dotenv_from(&cwd)
}

fn find_dotenv_from(start: &Path) -> Option<PathBuf> {
    let mut cursor = Some(start);
    while let Some(current) = cursor {
        let candidate = current.join(".env");
        if candidate.is_file() {
            return Some(candidate);
        }
        cursor = current.parent();
    }
    None
}
