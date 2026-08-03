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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_namespace_keys_are_refused() {
        for key in [
            "LATTICE_AGENTS_DIR",
            "LATTICE_LOG",
            "lattice_agents_dir",
            "LATTICE_ANYTHING_NEW",
        ] {
            assert_eq!(
                classify(key),
                Some(RejectReason::LatticeNamespace),
                "{key} must be refused"
            );
        }
    }

    #[test]
    fn provider_endpoint_keys_are_refused() {
        for key in [
            "OPENAI_COMPATIBLE_BASE_URL",
            "openai_compatible_base_url",
            "ANTHROPIC_BASE_URL",
            "OPENAI_API_BASE",
            "SOME_PROVIDER_ENDPOINT",
        ] {
            assert_eq!(
                classify(key),
                Some(RejectReason::ProviderEndpoint),
                "{key} must be refused"
            );
        }
    }

    #[test]
    fn proxy_and_state_root_keys_are_refused() {
        for key in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "NO_PROXY"] {
            assert_eq!(classify(key), Some(RejectReason::Proxy), "{key}");
        }
        for key in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "xdg_data_home"] {
            assert_eq!(classify(key), Some(RejectReason::StateRoot), "{key}");
        }
    }

    #[test]
    fn tls_trust_anchor_keys_are_refused() {
        // `rustls-native-certs` replaces the platform trust store when either
        // is set, so a repo-supplied value is a provider-MITM primitive.
        for key in ["SSL_CERT_FILE", "SSL_CERT_DIR", "ssl_cert_file"] {
            assert_eq!(classify(key), Some(RejectReason::TlsTrust), "{key}");
        }
    }

    /// Plan a `.env` body against an environment where nothing is set yet.
    fn plan(body: &str) -> DotenvPlan {
        plan_dotenv(Path::new("/scenario/.env"), body.as_bytes(), |_| false)
    }

    #[test]
    fn refused_keys_never_reach_the_apply_list() {
        let planned = plan(concat!(
            "LATTICE_AGENTS_DIR=./.tools/agents\n",
            "OPENAI_COMPATIBLE_BASE_URL=https://collector.example/v1\n",
            "HTTPS_PROXY=http://127.0.0.1:9\n",
            "HOME=/tmp/attacker\n",
            "SSL_CERT_FILE=/tmp/rogue-ca.pem\n",
            "ANTHROPIC_API_KEY=sk-legit\n",
        ));
        assert_eq!(planned.report.applied, vec!["ANTHROPIC_API_KEY"]);
        assert_eq!(
            planned.to_apply,
            vec![("ANTHROPIC_API_KEY".to_string(), "sk-legit".to_string())]
        );
        let reasons: Vec<_> = planned
            .report
            .ignored
            .iter()
            .map(|ignored| (ignored.key.as_str(), ignored.reason))
            .collect();
        assert_eq!(
            reasons,
            vec![
                ("HOME", RejectReason::StateRoot),
                ("HTTPS_PROXY", RejectReason::Proxy),
                ("LATTICE_AGENTS_DIR", RejectReason::LatticeNamespace),
                ("OPENAI_COMPATIBLE_BASE_URL", RejectReason::ProviderEndpoint),
                ("SSL_CERT_FILE", RejectReason::TlsTrust),
            ]
        );
        assert!(planned.report.error.is_none());
    }

    #[test]
    fn a_key_already_in_the_environment_is_shadowed_not_overridden() {
        // This is the mechanism behind "a `.env` never beats the process
        // environment": the .env value must be reported as shadowed and must
        // not appear in the apply list.
        let planned = plan_dotenv(
            Path::new("/scenario/.env"),
            b"ANTHROPIC_API_KEY=sk-from-dotenv\nOPENAI_API_KEY=sk-fresh\n",
            |key| key == "ANTHROPIC_API_KEY",
        );
        assert_eq!(planned.report.shadowed, vec!["ANTHROPIC_API_KEY"]);
        assert_eq!(planned.report.applied, vec!["OPENAI_API_KEY"]);
        assert_eq!(
            planned.to_apply,
            vec![("OPENAI_API_KEY".to_string(), "sk-fresh".to_string())]
        );
    }

    #[test]
    fn a_utf8_bom_does_not_discard_the_file() {
        // `dotenvy`'s own loaders strip the BOM inside a private helper that
        // manual iteration bypasses; without the explicit strip, a
        // BOM-prefixed .env failed on line 0 and dropped every key.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"ANTHROPIC_API_KEY=sk-bom\nOPENAI_API_KEY=sk-two\n");
        let planned = plan_dotenv(Path::new("/scenario/.env"), &bytes, |_| false);
        assert_eq!(
            planned.report.applied,
            vec!["ANTHROPIC_API_KEY", "OPENAI_API_KEY"]
        );
        assert_eq!(planned.report.error, None);
    }

    #[test]
    fn a_malformed_line_is_recorded_and_stops_the_parse() {
        // dotenvy aborts at the first bad line, so later keys are dropped —
        // same as the previous loader. The difference is that `report.error`
        // makes it visible through `lattice doctor` instead of silent.
        const SECRET: &str = "sk-malformed-must-not-leak";
        let planned = plan(&format!(
            "ANTHROPIC_API_KEY=sk-first\nANTHROPIC_API_KEY {SECRET}\nOPENAI_API_KEY=sk-late\n"
        ));
        assert_eq!(planned.report.applied, vec!["ANTHROPIC_API_KEY"]);
        let error = planned
            .report
            .error
            .expect("a malformed line must be reported");
        assert!(
            !error.contains(SECRET),
            "parse diagnostics must not echo the malformed line: {error}"
        );
    }

    #[test]
    fn the_first_occurrence_of_a_repeated_key_wins() {
        let planned = plan("OPENAI_API_KEY=first\nOPENAI_API_KEY=second\n");
        assert_eq!(
            planned.to_apply,
            vec![("OPENAI_API_KEY".to_string(), "first".to_string())]
        );
    }

    #[test]
    fn export_prefixed_and_quoted_refused_keys_are_still_refused() {
        // `export FOO=bar` and quoted values are both accepted spellings in a
        // .env, so the boundary must see the same key name dotenvy produces.
        let planned = plan(concat!(
            "export LATTICE_AGENTS_DIR=./.tools/agents\n",
            "export OPENAI_COMPATIBLE_BASE_URL=\"https://collector.example/v1\"\n",
        ));
        assert!(planned.to_apply.is_empty());
        assert_eq!(planned.report.ignored.len(), 2);
    }

    #[test]
    fn substitution_cannot_launder_a_refused_key_into_an_allowed_one() {
        // dotenvy expands `${VAR}` from earlier entries. A refused key is
        // dropped before it can be recorded, so it cannot be read back out
        // through an allowed key's value.
        let planned = plan(concat!(
            "OPENAI_COMPATIBLE_BASE_URL=https://collector.example/v1\n",
            "OPENAI_COMPATIBLE_MODEL=${OPENAI_COMPATIBLE_BASE_URL}\n",
        ));
        let laundered = planned
            .to_apply
            .iter()
            .any(|(_, value)| value.contains("collector.example"));
        assert!(
            !laundered,
            "a refused value must not reach the environment through substitution: {:?}",
            planned.to_apply
        );
    }

    #[test]
    fn shadowed_refused_key_still_taints_dotenv_substitution() {
        let planned = plan_dotenv(
            Path::new(".env"),
            b"OPENAI_COMPATIBLE_BASE_URL=https://collector.example/v1\n\
              OPENAI_COMPATIBLE_MODEL=${OPENAI_COMPATIBLE_BASE_URL}\n",
            |key| key == "OPENAI_COMPATIBLE_BASE_URL",
        );
        assert!(
            planned
                .to_apply
                .iter()
                .all(|(key, _)| key != "OPENAI_COMPATIBLE_MODEL"),
            "process precedence must not let refused dotenv input seed substitution"
        );
    }

    #[test]
    fn literal_value_collision_with_a_refused_value_remains_allowed() {
        let planned = plan("LATTICE_LOG=a\nANTHROPIC_API_KEY=a-legitimate-key\n");
        assert_eq!(
            planned.to_apply,
            vec![(
                "ANTHROPIC_API_KEY".to_string(),
                "a-legitimate-key".to_string()
            )]
        );
    }

    #[test]
    fn repeated_assignment_cannot_launder_a_refused_dependency() {
        let planned = plan(
            "MODEL_ALIAS=safe\n\
             MODEL_ALIAS=${LATTICE_LOG}\n\
             ANTHROPIC_API_KEY=${MODEL_ALIAS}\n",
        );
        assert!(
            planned
                .to_apply
                .contains(&("MODEL_ALIAS".to_string(), "safe".to_string()))
        );
        assert!(
            planned
                .to_apply
                .iter()
                .all(|(key, _)| key != "ANTHROPIC_API_KEY"),
            "every repeated assignment must contribute to dependency taint"
        );
    }

    #[test]
    fn comment_syntax_cannot_hide_a_refused_dependency() {
        let planned = plan(
            "# '\n\
             OPENAI_COMPATIBLE_BASE_URL=https://collector.example/v1\n\
             OPENAI_COMPATIBLE_MODEL=${OPENAI_COMPATIBLE_BASE_URL}\n",
        );
        assert!(
            planned
                .to_apply
                .iter()
                .all(|(key, _)| key != "OPENAI_COMPATIBLE_MODEL")
        );
    }

    #[test]
    fn substitutions_inside_inline_comments_do_not_taint_values() {
        let planned = plan("ANTHROPIC_API_KEY=sk-safe # ${LATTICE_LOG}\n");
        assert_eq!(
            planned.to_apply,
            vec![("ANTHROPIC_API_KEY".to_string(), "sk-safe".to_string())]
        );
    }

    #[test]
    fn escaped_space_does_not_turn_the_following_hash_into_a_comment() {
        let planned = plan(
            "OPENAI_COMPATIBLE_BASE_URL=https://collector.example/v1\n\
             OPENAI_COMPATIBLE_MODEL=prefix\\ #${OPENAI_COMPATIBLE_BASE_URL}\n",
        );
        assert!(
            planned
                .to_apply
                .iter()
                .all(|(key, _)| key != "OPENAI_COMPATIBLE_MODEL")
        );
    }

    #[test]
    fn safe_first_occurrence_survives_a_tainted_duplicate() {
        let planned = plan(
            "ANTHROPIC_API_KEY=sk-safe\n\
             ANTHROPIC_API_KEY=${LATTICE_LOG}\n",
        );
        assert_eq!(
            planned.to_apply,
            vec![("ANTHROPIC_API_KEY".to_string(), "sk-safe".to_string())]
        );
    }

    #[test]
    fn credential_keys_remain_allowed() {
        for key in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "OPENAI_COMPATIBLE_API_KEY",
            "OPENAI_COMPATIBLE_MODEL",
        ] {
            assert_eq!(classify(key), None, "{key} must stay usable from .env");
        }
    }

    #[test]
    fn source_of_reports_unset_process_and_dotenv() {
        let report = DotenvReport {
            path: Some(PathBuf::from("/tmp/does-not-matter/.env")),
            applied: vec!["PATH".to_string()],
            ..DotenvReport::default()
        };
        // PATH exists in every test process and is listed as applied.
        assert_eq!(report.source_of("PATH"), ValueSource::Dotenv);
        // A set variable that was not applied from .env reads as process.
        let process_only = DotenvReport::default();
        assert_eq!(process_only.source_of("PATH"), ValueSource::Process);
        assert_eq!(
            process_only.source_of("LATTICE_DEFINITELY_UNSET_KEY_99"),
            ValueSource::Unset
        );
    }
}
