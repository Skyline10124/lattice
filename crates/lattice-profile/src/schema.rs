// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lancet_protocol::BoundaryDimension;
use lancet_protocol::CepMode;
use lancet_protocol::ImpactKind;
use serde::{Deserialize, Serialize, de};
use std::collections::BTreeMap;
use std::path::{Component, Path};

/// Strict v1 `agent.toml` profile. Only `schema_version = 1` is accepted.
///
/// Removed sections: `[security]`, `[context]`, `[behavior]`.
/// The `[tools]` section only supports explicit `enabled` — no `bundles`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    pub schema_version: u64,
    pub agent: AgentSection,
    pub prompt: PromptSection,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub llm: LlmSection,
    #[serde(default)]
    pub lancet: LancetSection,
    /// Upper workspace execution environment configuration.
    ///
    /// Absent `[workspace]` means no Upper is requested; the field defaults to `None`.
    /// Older profiles without `[workspace]` still parse (EGRESS-01 / CRED-01 profile layer).
    #[serde(default)]
    pub workspace: Option<WorkspaceSection>,
}

// ── Workspace schema types ────────────────────────────────────────────────────

/// Egress network policy for the Upper VM (EGRESS-01 profile layer).
///
/// Controls which outbound traffic the Upper VM permits. Defaults to `open` when the
/// `egress` field is absent from `[workspace]`.
///
/// - `open`      — full public internet access (SDK default)
/// - `blocked`   — deny all outbound traffic
/// - `allowlist` — deny all except `allowed_egress_targets` (domain names)
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressPolicyConfig {
    /// Full public internet access (SDK default). Implicit when `egress` is absent.
    #[default]
    Open,
    /// Deny all outbound traffic.
    Blocked,
    /// Deny all traffic except explicitly listed domains in `allowed_egress_targets`.
    Allowlist,
}

/// Reference to a credential by its environment variable name (CRED-01 profile layer).
///
/// **Security invariant:** this type stores only the *name* of the environment variable.
/// The resolved secret value is NEVER stored here — it is injected at turn time by the
/// Upper provisioner (`lattice-workspace`). Snapshots and profile files must never
/// contain actual secrets.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRef {
    /// Name of the environment variable that holds the secret at turn time
    /// (e.g. `"OPENAI_API_KEY"`). The resolved value is never stored here.
    pub env_var_name: String,
}

// ── Workspace lifecycle limit constants ─────────────────────────────────────

/// Default number of cleanup rounds before the host exhausts the budget (D-09).
/// Valid range: `1..=32`.
pub const DEFAULT_CLEANUP_ROUNDS: u32 = 3;

/// Default maximum pending snapshots per session (D-17).
/// Valid range: `1..=100`.
pub const DEFAULT_MAX_PENDING_SNAPSHOTS: u32 = 5;

fn default_cleanup_rounds() -> u32 {
    DEFAULT_CLEANUP_ROUNDS
}

fn default_max_pending_snapshots() -> u32 {
    DEFAULT_MAX_PENDING_SNAPSHOTS
}

fn deserialize_bounded_cleanup_rounds<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u32::deserialize(deserializer)?;
    if !(1..=32).contains(&value) {
        return Err(de::Error::custom(format!(
            "cleanup_rounds must be in 1..=32, got {value}"
        )));
    }
    Ok(value)
}

fn deserialize_bounded_max_pending_snapshots<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u32::deserialize(deserializer)?;
    if !(1..=100).contains(&value) {
        return Err(de::Error::custom(format!(
            "max_pending_snapshots must be in 1..=100, got {value}"
        )));
    }
    Ok(value)
}

/// `[workspace]` — Upper VM execution environment configuration.
///
/// Controls egress policy, credential references, resource limits, and the OCI image
/// (or pre-warmed template name) for the Upper workspace provisioned per turn.
///
/// Most fields are `Option<T>` with `#[serde(default)]` so missing fields fall back to
/// caller defaults. Lifecycle limit fields (`cleanup_rounds`, `max_pending_snapshots`)
/// are non-optional `u32` with bounded defaults. Older profiles without `[workspace]`
/// still parse and produce `AgentProfile.workspace = None`.
///
/// **Security invariant (T-33-04):** credentials are turn-time injection only.
/// This section stores only references (`CredentialRef`); resolved values are never
/// present in profile or session snapshot data.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSection {
    /// Egress policy for the Upper VM. Defaults to `open` when absent.
    #[serde(default)]
    pub egress: Option<EgressPolicyConfig>,
    /// Credential references injected into the Upper at turn time.
    ///
    /// Each entry names an environment variable; the resolved value is fetched at
    /// provisioning time, never stored in the profile.
    #[serde(default)]
    pub credentials: Vec<CredentialRef>,
    /// OCI image reference or pre-warmed template name for the Upper VM.
    ///
    /// Use a fully-qualified OCI ref (e.g. `"docker.io/library/python:3.12-slim"`)
    /// or a template name stored under `~/.microsandbox/snapshots/`. When absent,
    /// hosts must supply an explicit image or template source for Direct Upper
    /// preparation — no provisioner default is implied by the profile schema.
    #[serde(default)]
    pub image: Option<String>,
    /// Number of virtual CPUs to allocate to the Upper VM. Defaults to provisioner default.
    #[serde(default)]
    pub cpus: Option<u32>,
    /// Memory in MiB to allocate to the Upper VM. Defaults to provisioner default.
    #[serde(default)]
    pub memory_mb: Option<u32>,
    /// Domain names permitted when `egress = "allowlist"`.
    ///
    /// Entries are domain name strings (e.g. `"api.openai.com"`). Ignored when
    /// `egress` is `open` or `blocked`.
    #[serde(default)]
    pub allowed_egress_targets: Vec<String>,
    /// Number of agent cleanup rounds before the host exhausts the budget (D-09).
    ///
    /// When the configured limit is exhausted, the cleanup callback returns `None`
    /// and the orchestrator sets disposition to `pending_worktree`.
    /// Valid range: `1..=32`. Default: `3`.
    #[serde(
        default = "default_cleanup_rounds",
        deserialize_with = "deserialize_bounded_cleanup_rounds"
    )]
    pub cleanup_rounds: u32,
    /// Maximum number of pending retained snapshots per session (D-17).
    ///
    /// When the quota is exceeded, new snapshot retention is refused.
    /// Valid range: `1..=100`. Default: `5`.
    #[serde(
        default = "default_max_pending_snapshots",
        deserialize_with = "deserialize_bounded_max_pending_snapshots"
    )]
    pub max_pending_snapshots: u32,
}

impl Default for WorkspaceSection {
    fn default() -> Self {
        Self {
            egress: None,
            credentials: Vec::new(),
            image: None,
            cpus: None,
            memory_mb: None,
            allowed_egress_targets: Vec::new(),
            cleanup_rounds: DEFAULT_CLEANUP_ROUNDS,
            max_pending_snapshots: DEFAULT_MAX_PENDING_SNAPSHOTS,
        }
    }
}

/// LANCET `[lancet]` profile section with typed policy sub-tables.
///
/// Known sub-tables (`project_root`, `observation`, `network`) deserialize into
/// typed fields. Other future sub-tables (e.g. `[lancet.command]`) remain in
/// `extensions`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LancetSection {
    #[serde(default = "default_lancet_mode")]
    pub mode: CepMode,
    #[serde(default)]
    pub project_root: Option<LancetProjectRootSection>,
    #[serde(default)]
    pub observation: Option<LancetObservationSection>,
    #[serde(default)]
    pub network: Option<LancetNetworkSection>,
    #[serde(default)]
    pub command: Option<LancetCommandSection>,
    #[serde(default)]
    pub policy: Option<LancetPolicySection>,
    #[serde(default)]
    pub isolation: Option<LancetIsolationSection>,
    #[serde(default)]
    pub effects: Option<LancetEffectsSection>,
    #[serde(default)]
    pub workspace: Option<LancetWorkspacePolicySection>,
    #[serde(default)]
    pub verify: Option<LancetVerifySection>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, toml::Value>,
}

/// `[lancet.project_root]` — effective workspace scope for observation policy.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetProjectRootSection {
    #[serde(default)]
    pub root: Option<String>,
}

/// `[lancet.observation]` — path allow roots and observation/todo budget limits.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetObservationSection {
    #[serde(default)]
    pub allowed_roots: Vec<String>,
    pub max_single_read_bytes: Option<usize>,
    pub max_turn_observation_bytes: Option<usize>,
    pub max_grep_matches: Option<usize>,
    pub max_list_entries: Option<usize>,
    pub max_find_paths: Option<usize>,
    pub max_todo_ops: Option<usize>,
}

/// `[lancet.network]` — `fetch.web` host policy and response size cap.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetNetworkSection {
    pub allow_public_https: Option<bool>,
    pub allow_localhost: Option<bool>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    pub max_web_response_bytes: Option<usize>,
}

/// `[lancet.command]` — command execution policy overrides.
///
/// All fields are `Option<T>` with `#[serde(default)]` so missing fields
/// fall back to `CommandPolicy::defaults()`. Missing `[lancet.command]` entirely
/// uses all defaults. Older profiles without `[lancet.command]` still parse.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetCommandSection {
    #[serde(default)]
    pub default_timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
    #[serde(default)]
    pub allow_inline_shell: Option<bool>,
    #[serde(default)]
    pub inline_shell_max_script_bytes: Option<usize>,
    #[serde(default)]
    pub inline_shell_max_commands: Option<usize>,
    #[serde(default)]
    pub inline_shell_max_words_per_command: Option<usize>,
    #[serde(default)]
    pub allowed_diagnostic_commands: Option<Vec<Vec<String>>>,
    #[serde(default)]
    pub denied_commands: Option<Vec<Vec<String>>>,
    #[serde(default)]
    pub allowed_env: Option<Vec<String>>,
    #[serde(default)]
    pub inherited_env: Option<Vec<String>>,
}

/// `[lancet.policy]` — pointer to an external structured TOML policy file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetPolicySection {
    #[serde(default)]
    pub path: Option<String>,
}

/// `[lancet.workspace]` — immutable Direct write-region policy.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetWorkspacePolicySection {
    #[serde(default)]
    pub allowed_write_paths: Vec<String>,
}

/// `[lancet.verify]` — immutable Direct post-mutation verify command.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LancetVerifySection {
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub expected_exit_code: Option<i32>,
}

/// `[lancet.isolation]` — physical boundary configuration namespace.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct LancetIsolationSection {
    #[serde(default)]
    pub command: Option<LancetCommandIsolationSection>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, toml::Value>,
}

impl<'de> Deserialize<'de> for LancetIsolationSection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut raw = BTreeMap::<String, toml::Value>::deserialize(deserializer)?;
        for (key, value) in &raw {
            if !matches!(value, toml::Value::Table(_)) {
                return Err(de::Error::custom(format!(
                    "unknown scalar field `lancet.isolation.{key}`; only typed or future sub-tables are supported"
                )));
            }
        }
        let command = raw
            .remove("command")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        Ok(Self {
            command,
            extensions: raw,
        })
    }
}

/// `[lancet.isolation.command]` — command physical-boundary policy.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LancetCommandIsolationSection {
    pub mode: LancetIsolationMode,
    pub required_boundaries: Vec<BoundaryDimension>,
    pub optional_boundaries: Vec<BoundaryDimension>,
    pub read_only_toolchain_paths: Vec<String>,
    pub scratch_paths: Vec<String>,
    pub landlock: LancetLandlockBehavior,
    pub unsupported_host: LancetUnsupportedHostBehavior,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLancetCommandIsolationSection {
    pub mode: LancetIsolationMode,
    #[serde(default)]
    pub required_boundaries: Vec<BoundaryDimension>,
    #[serde(default)]
    pub optional_boundaries: Vec<BoundaryDimension>,
    #[serde(default)]
    pub read_only_toolchain_paths: Vec<String>,
    #[serde(default)]
    pub scratch_paths: Vec<String>,
    #[serde(default = "default_lancet_landlock_behavior")]
    pub landlock: LancetLandlockBehavior,
    #[serde(default = "default_lancet_unsupported_host_behavior")]
    pub unsupported_host: LancetUnsupportedHostBehavior,
}

impl<'de> Deserialize<'de> for LancetCommandIsolationSection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawLancetCommandIsolationSection::deserialize(deserializer)?;
        let section = Self {
            mode: raw.mode,
            required_boundaries: raw.required_boundaries,
            optional_boundaries: raw.optional_boundaries,
            read_only_toolchain_paths: raw.read_only_toolchain_paths,
            scratch_paths: raw.scratch_paths,
            landlock: raw.landlock,
            unsupported_host: raw.unsupported_host,
        };
        section.validate().map_err(de::Error::custom)?;
        Ok(section)
    }
}

impl LancetCommandIsolationSection {
    fn validate(&self) -> Result<(), String> {
        validate_boundary_list("required_boundaries", &self.required_boundaries)?;
        validate_boundary_list("optional_boundaries", &self.optional_boundaries)?;
        for boundary in &self.required_boundaries {
            if self.optional_boundaries.contains(boundary) {
                return Err(format!(
                    "boundary `{}` cannot appear in both required_boundaries and optional_boundaries",
                    boundary.label()
                ));
            }
        }

        if self.landlock == LancetLandlockBehavior::Off
            && self
                .required_boundaries
                .contains(&BoundaryDimension::Landlock)
        {
            return Err(
                "landlock = \"off\" contradicts required_boundaries containing `landlock`"
                    .to_string(),
            );
        }

        validate_unsupported_host_behavior(self.mode, self.unsupported_host)?;
        validate_path_list("read_only_toolchain_paths", &self.read_only_toolchain_paths)?;
        validate_path_list("scratch_paths", &self.scratch_paths)?;
        Ok(())
    }
}

fn validate_boundary_list(field: &str, boundaries: &[BoundaryDimension]) -> Result<(), String> {
    let mut seen = Vec::new();
    for boundary in boundaries {
        if seen.contains(boundary) {
            return Err(format!(
                "duplicate boundary `{}` in {field}",
                boundary.label()
            ));
        }
        seen.push(*boundary);
    }
    Ok(())
}

fn validate_unsupported_host_behavior(
    mode: LancetIsolationMode,
    behavior: LancetUnsupportedHostBehavior,
) -> Result<(), String> {
    let allowed = match mode {
        LancetIsolationMode::Enforce => LancetUnsupportedHostBehavior::FailClosed,
        LancetIsolationMode::Audit => LancetUnsupportedHostBehavior::AllowUnbounded,
        LancetIsolationMode::Off => LancetUnsupportedHostBehavior::NotRequested,
    };
    if behavior == LancetUnsupportedHostBehavior::ModeDefault || behavior == allowed {
        return Ok(());
    }
    Err(format!(
        "unsupported_host = \"{}\" contradicts mode = \"{}\"; use \"mode_default\" or \"{}\"",
        behavior.label(),
        mode.label(),
        allowed.label()
    ))
}

fn validate_path_list(field: &str, paths: &[String]) -> Result<(), String> {
    for path in paths {
        if path.trim().is_empty() {
            return Err(format!("{field} entries must be nonempty"));
        }
        if path.contains('\0') {
            return Err(format!("{field} entries must not contain NUL bytes"));
        }
        if Path::new(path)
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(format!(
                "{field} entry `{path}` must not contain parent-directory traversal"
            ));
        }
    }
    Ok(())
}

/// Physical-boundary enforcement mode for `exec.command_run`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LancetIsolationMode {
    Enforce,
    Audit,
    Off,
}

impl LancetIsolationMode {
    pub const fn label(self) -> &'static str {
        match self {
            LancetIsolationMode::Enforce => "enforce",
            LancetIsolationMode::Audit => "audit",
            LancetIsolationMode::Off => "off",
        }
    }
}

/// Landlock filesystem-refinement behavior for command isolation.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LancetLandlockBehavior {
    Auto,
    Off,
}

/// Unsupported-host behavior for requested command physical boundaries.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LancetUnsupportedHostBehavior {
    ModeDefault,
    FailClosed,
    AllowUnbounded,
    NotRequested,
}

impl LancetUnsupportedHostBehavior {
    pub const fn label(self) -> &'static str {
        match self {
            LancetUnsupportedHostBehavior::ModeDefault => "mode_default",
            LancetUnsupportedHostBehavior::FailClosed => "fail_closed",
            LancetUnsupportedHostBehavior::AllowUnbounded => "allow_unbounded",
            LancetUnsupportedHostBehavior::NotRequested => "not_requested",
        }
    }
}

fn default_lancet_landlock_behavior() -> LancetLandlockBehavior {
    LancetLandlockBehavior::Auto
}

fn default_lancet_unsupported_host_behavior() -> LancetUnsupportedHostBehavior {
    LancetUnsupportedHostBehavior::ModeDefault
}

/// `[lancet.effects]` — runtime effect-kind deny switch.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetEffectsSection {
    #[serde(default)]
    pub deny: Vec<ImpactKind>,
}

fn default_lancet_mode() -> CepMode {
    CepMode::Enforce
}

impl Default for LancetSection {
    fn default() -> Self {
        Self {
            mode: default_lancet_mode(),
            project_root: None,
            observation: None,
            network: None,
            command: None,
            policy: None,
            isolation: None,
            effects: None,
            workspace: None,
            verify: None,
            extensions: BTreeMap::new(),
        }
    }
}

impl<'de> Deserialize<'de> for LancetSection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut raw = BTreeMap::<String, toml::Value>::deserialize(deserializer)?;
        let mode = match raw.remove("mode") {
            Some(value) => value.try_into().map_err(de::Error::custom)?,
            None => default_lancet_mode(),
        };

        for (key, value) in &raw {
            if !matches!(value, toml::Value::Table(_)) {
                return Err(de::Error::custom(format!(
                    "unknown scalar field `lancet.{key}`; only `mode` and future sub-tables are supported"
                )));
            }
        }

        let project_root = raw
            .remove("project_root")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let observation = raw
            .remove("observation")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let network = raw
            .remove("network")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let command = raw
            .remove("command")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let policy = raw
            .remove("policy")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let isolation = raw
            .remove("isolation")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let effects = raw
            .remove("effects")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let workspace = raw
            .remove("workspace")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let verify = raw
            .remove("verify")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;

        Ok(Self {
            mode,
            project_root,
            observation,
            network,
            command,
            policy,
            isolation,
            effects,
            workspace,
            verify,
            extensions: raw,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PromptSection {
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub system_path: Option<String>,
}

/// Tool section: only explicit `enabled` tool names are supported.
///
/// `bundles` and any other field are rejected by `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    #[serde(default)]
    pub enabled: Vec<String>,
}

/// LLM provider execution settings.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LlmSection {
    #[serde(default)]
    pub retry: LlmRetrySection,
}

/// `[llm.retry]` — retry policy for opening provider streams.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LlmRetrySection {
    #[serde(default = "default_llm_retry_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_llm_retry_initial_delay_ms")]
    pub initial_delay_ms: u64,
    #[serde(default = "default_llm_retry_max_delay_ms")]
    pub max_delay_ms: u64,
}

impl Default for LlmRetrySection {
    fn default() -> Self {
        Self {
            max_retries: default_llm_retry_max_retries(),
            initial_delay_ms: default_llm_retry_initial_delay_ms(),
            max_delay_ms: default_llm_retry_max_delay_ms(),
        }
    }
}

fn default_llm_retry_max_retries() -> u32 {
    3
}

fn default_llm_retry_initial_delay_ms() -> u64 {
    500
}

fn default_llm_retry_max_delay_ms() -> u64 {
    3_000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> Result<AgentProfile, toml::de::Error> {
        toml::from_str::<AgentProfile>(toml_str)
    }

    #[test]
    fn valid_minimal_profile() {
        let toml = r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "You are a helpful assistant."
"#;
        let profile = parse(toml).unwrap();
        assert_eq!(profile.schema_version, 1);
        assert_eq!(profile.agent.name, "test");
        assert_eq!(profile.agent.model, "sonnet");
        assert!(profile.agent.description.is_none());
        assert_eq!(
            profile.prompt.system.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert!(profile.prompt.system_path.is_none());
    }

    #[test]
    fn valid_full_profile() {
        let toml = r#"
schema_version = 1

[agent]
name = "full"
model = "opus"
description = "A full-featured agent"

[prompt]
system_path = "prompt.md"

[tools]
enabled = ["observe.workspace", "exec.command_run"]

[llm.retry]
max_retries = 2
initial_delay_ms = 250
max_delay_ms = 1000
"#;
        let profile = parse(toml).unwrap();
        assert_eq!(
            profile.agent.description.as_deref(),
            Some("A full-featured agent")
        );
        assert_eq!(profile.prompt.system_path.as_deref(), Some("prompt.md"));
        let tools = &profile.tools;
        assert_eq!(tools.enabled, vec!["observe.workspace", "exec.command_run"]);
        assert_eq!(profile.llm.retry.max_retries, 2);
        assert_eq!(profile.llm.retry.initial_delay_ms, 250);
        assert_eq!(profile.llm.retry.max_delay_ms, 1000);
    }

    // ── Top-level section rejection ───────────────────────────────────

    #[test]
    fn reject_unknown_top_level_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[orchestrator]
some_field = "bad"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("orchestrator"), "got: {err}");
    }

    #[test]
    fn reject_security_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[security]
sandbox_mode = "project"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("security"), "got: {err}");
    }

    #[test]
    fn reject_context_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[context]
project_instructions = true
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("context"), "got: {err}");
    }

    #[test]
    fn reject_behavior_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[behavior]
max_turns = 10
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("behavior"), "got: {err}");
    }

    #[test]
    fn reject_capabilities_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[capabilities]
presets = ["coding"]
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("capabilities"), "got: {err}");
    }

    #[test]
    fn reject_handoff_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[handoff]
enabled = true
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_all_forbidden_top_level_sections() {
        let forbidden = [
            "capabilities",
            "handoff",
            "orchestrator",
            "plugins",
            "pipeline",
            "subagents",
            "micro_agents",
            "memory",
            "security",
            "context",
            "behavior",
        ];
        for section in forbidden {
            let toml = format!(
                r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[{section}]
x = "y"
"#
            );
            assert!(
                parse(&toml).is_err(),
                "section [{section}] should be rejected"
            );
        }
    }

    // ── Tools section rejection ───────────────────────────────────────

    #[test]
    fn reject_tools_bundles() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[tools]
bundles = ["workspace"]
"#;
        let err = parse(toml).unwrap_err();
        assert!(
            err.to_string().contains("bundles") || err.to_string().contains("unknown field"),
            "bundles must be rejected, got: {err}"
        );
    }

    // ── Agent field rejection ─────────────────────────────────────────

    #[test]
    fn reject_agent_preset_field() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"
preset = "coding"

[prompt]
system = "hi"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("preset"), "got: {err}");
    }

    #[test]
    fn reject_agent_type_code_agent() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"
type = "code_agent"

[prompt]
system = "hi"
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_agent_skippable() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"
skippable = true

[prompt]
system = "hi"
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_agent_tags() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"
tags = ["coding"]

[prompt]
system = "hi"
"#;
        assert!(parse(toml).is_err());
    }

    // ── CEP section ───────────────────────────────────────────────────

    fn minimal_profile_prefix() -> &'static str {
        r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"
"#
    }

    #[test]
    fn cep_defaults_to_enforce_when_section_absent() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
        assert!(profile.lancet.extensions.is_empty());
    }

    #[test]
    fn cep_parses_enforce_mode() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
    }

    #[test]
    fn cep_parses_warn_mode() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "warn"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Warn);
    }

    #[test]
    fn cep_parses_off_mode() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "off"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Off);
    }

    #[test]
    fn cep_rejects_invalid_mode() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "permissive"
"#,
            prefix = minimal_profile_prefix()
        );
        assert!(parse(&toml).is_err());
    }

    #[test]
    fn cep_rejects_scalar_extension_typos() {
        let toml = format!(
            r#"{prefix}

[lancet]
mod = "off"
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(err.to_string().contains("lancet.mod"), "got: {err}");
    }

    #[test]
    fn cep_parses_typed_prof02_sub_tables() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.project_root]
root = "."

[lancet.observation]
allowed_roots = ["."]
max_single_read_bytes = 4096
max_turn_observation_bytes = 8192
max_grep_matches = 500
max_list_entries = 300
max_find_paths = 400
max_todo_ops = 10

[lancet.network]
allow_public_https = true
allow_localhost = false
allowed_hosts = ["example.com"]
max_web_response_bytes = 2048
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
        let project_root = profile.lancet.project_root.as_ref().unwrap();
        assert_eq!(project_root.root.as_deref(), Some("."));
        let observation = profile.lancet.observation.as_ref().unwrap();
        assert_eq!(observation.allowed_roots, vec!["."]);
        assert_eq!(observation.max_single_read_bytes, Some(4096));
        assert_eq!(observation.max_turn_observation_bytes, Some(8192));
        assert_eq!(observation.max_grep_matches, Some(500));
        let network = profile.lancet.network.as_ref().unwrap();
        assert_eq!(network.allow_public_https, Some(true));
        assert_eq!(network.allow_localhost, Some(false));
        assert_eq!(network.allowed_hosts, vec!["example.com"]);
        assert_eq!(network.max_web_response_bytes, Some(2048));
    }

    #[test]
    fn cep_preserves_untyped_future_sub_tables_in_extensions() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.future_table]
some_field = true
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
        assert!(profile.lancet.extensions.contains_key("future_table"));
        assert!(profile.lancet.project_root.is_none());
        assert!(profile.lancet.observation.is_none());
        assert!(profile.lancet.network.is_none());
        assert!(profile.lancet.command.is_none());
    }

    #[test]
    fn cep_older_profile_mode_only_still_parses() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
        assert!(profile.lancet.project_root.is_none());
        assert!(profile.lancet.observation.is_none());
        assert!(profile.lancet.network.is_none());
    }

    #[test]
    fn cep_observation_rejects_network_only_field() {
        let toml = format!(
            r#"{prefix}

[lancet.observation]
max_web_response_bytes = 1024
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("max_web_response_bytes"),
            "got: {err}"
        );
    }

    // ── LancetCommandSection ────────────────────────────────────────────

    #[test]
    fn cep_command_section_parses_all_fields() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.command]
default_timeout_ms = 60000
max_timeout_ms = 180000
max_output_bytes = 100000
allow_inline_shell = true
inline_shell_max_script_bytes = 2048
inline_shell_max_commands = 8
inline_shell_max_words_per_command = 32
allowed_diagnostic_commands = [["cargo", "test"], ["git", "diff"]]
denied_commands = [["rm", "-rf"]]
allowed_env = ["RUST_LOG"]
inherited_env = ["PATH", "HOME"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let cmd = profile.lancet.command.as_ref().unwrap();
        assert_eq!(cmd.default_timeout_ms, Some(60000));
        assert_eq!(cmd.max_timeout_ms, Some(180000));
        assert_eq!(cmd.max_output_bytes, Some(100000));
        assert_eq!(cmd.allow_inline_shell, Some(true));
        assert_eq!(cmd.inline_shell_max_script_bytes, Some(2048));
        assert_eq!(cmd.inline_shell_max_commands, Some(8));
        assert_eq!(cmd.inline_shell_max_words_per_command, Some(32));
        assert_eq!(
            cmd.allowed_diagnostic_commands.as_ref().unwrap(),
            &vec![
                vec!["cargo".to_string(), "test".to_string()],
                vec!["git".to_string(), "diff".to_string()]
            ]
        );
        assert_eq!(
            cmd.denied_commands.as_ref().unwrap(),
            &vec![vec!["rm".to_string(), "-rf".to_string()]]
        );
        assert_eq!(
            cmd.allowed_env.as_ref().unwrap(),
            &vec!["RUST_LOG".to_string()]
        );
        assert_eq!(
            cmd.inherited_env.as_ref().unwrap(),
            &vec!["PATH".to_string(), "HOME".to_string()]
        );
    }

    #[test]
    fn cep_command_section_defaults_when_absent() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(profile.lancet.command.is_none());
    }

    #[test]
    fn cep_command_section_partial_fields_use_defaults() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.command]
default_timeout_ms = 30000
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let cmd = profile.lancet.command.as_ref().unwrap();
        assert_eq!(cmd.default_timeout_ms, Some(30000));
        assert!(cmd.max_timeout_ms.is_none());
        assert!(cmd.allow_inline_shell.is_none());
        assert!(cmd.allowed_diagnostic_commands.is_none());
    }

    #[test]
    fn cep_command_section_rejects_unknown_fields() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.command]
unknown_field = true
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("unknown_field"),
            "should reject unknown field, got: {err}"
        );
    }

    #[test]
    fn cep_command_section_old_profile_without_command_still_parses() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.observation]
allowed_roots = ["."]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert!(profile.lancet.command.is_none());
        assert!(profile.lancet.observation.is_some());
    }

    #[test]
    fn cep_command_section_moved_from_extensions_to_typed() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.command]
default_timeout_ms = 60000
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        // Should be in typed field, NOT in extensions
        assert!(profile.lancet.command.is_some());
        assert!(!profile.lancet.extensions.contains_key("command"));
    }

    // ── LancetIsolationSection ─────────────────────────────────────────

    #[test]
    fn cep_isolation_command_section_parses_required_boundaries() {
        let toml = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "enforce"
required_boundaries = ["network", "filesystem", "landlock"]
optional_boundaries = []
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let isolation = profile.lancet.isolation.as_ref().unwrap();
        let command = isolation.command.as_ref().unwrap();
        assert_eq!(command.mode, LancetIsolationMode::Enforce);
        assert_eq!(
            command.required_boundaries,
            vec![
                BoundaryDimension::Network,
                BoundaryDimension::Filesystem,
                BoundaryDimension::Landlock
            ]
        );
        assert!(command.optional_boundaries.is_empty());
    }

    #[test]
    fn cep_isolation_command_section_parses_full_contract() {
        let toml = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "audit"
required_boundaries = ["network", "filesystem"]
optional_boundaries = ["landlock"]
read_only_toolchain_paths = ["/usr", "/bin"]
scratch_paths = ["build/tmp", "/tmp/lattice"]
landlock = "auto"
unsupported_host = "allow_unbounded"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let command = profile
            .lancet
            .isolation
            .as_ref()
            .unwrap()
            .command
            .as_ref()
            .unwrap();
        assert_eq!(command.mode, LancetIsolationMode::Audit);
        assert_eq!(
            command.required_boundaries,
            vec![BoundaryDimension::Network, BoundaryDimension::Filesystem]
        );
        assert_eq!(
            command.optional_boundaries,
            vec![BoundaryDimension::Landlock]
        );
        assert_eq!(command.read_only_toolchain_paths, vec!["/usr", "/bin"]);
        assert_eq!(command.scratch_paths, vec!["build/tmp", "/tmp/lattice"]);
        assert_eq!(command.landlock, LancetLandlockBehavior::Auto);
        assert_eq!(
            command.unsupported_host,
            LancetUnsupportedHostBehavior::AllowUnbounded
        );
    }

    #[test]
    fn cep_isolation_command_section_parses_all_modes() {
        for (raw_mode, expected) in [
            ("enforce", LancetIsolationMode::Enforce),
            ("audit", LancetIsolationMode::Audit),
            ("off", LancetIsolationMode::Off),
        ] {
            let toml = format!(
                r#"{prefix}

[lancet.isolation.command]
mode = "{raw_mode}"
"#,
                prefix = minimal_profile_prefix()
            );
            let profile = parse(&toml).unwrap();
            let command = profile
                .lancet
                .isolation
                .as_ref()
                .unwrap()
                .command
                .as_ref()
                .unwrap();
            assert_eq!(command.mode, expected);
        }
    }

    #[test]
    fn cep_isolation_command_section_rejects_unknown_field() {
        let toml = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "enforce"
required_boundaries = ["network"]
unknown = true
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("unknown"),
            "should reject unknown isolation command field, got: {err}"
        );
    }

    #[test]
    fn cep_isolation_command_section_rejects_unknown_boundary() {
        let toml = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "enforce"
required_boundaries = ["seccomp"]
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("seccomp"),
            "should reject unknown boundary name, got: {err}"
        );
    }

    #[test]
    fn cep_isolation_command_rejects_required_landlock_when_disabled() {
        let toml = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "enforce"
required_boundaries = ["landlock"]
landlock = "off"
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("landlock"),
            "should reject disabled required Landlock, got: {err}"
        );
    }

    #[test]
    fn cep_isolation_command_rejects_duplicate_boundary_declarations() {
        let duplicate_required = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "enforce"
required_boundaries = ["network", "network"]
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&duplicate_required).unwrap_err();
        assert!(
            err.to_string().contains("duplicate"),
            "should reject duplicate required boundary, got: {err}"
        );

        let required_optional_overlap = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "audit"
required_boundaries = ["filesystem"]
optional_boundaries = ["filesystem"]
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&required_optional_overlap).unwrap_err();
        assert!(
            err.to_string().contains("optional"),
            "should reject required/optional overlap, got: {err}"
        );
    }

    #[test]
    fn cep_isolation_command_rejects_unsupported_host_mode_contradictions() {
        for (mode, unsupported_host) in [
            ("enforce", "allow_unbounded"),
            ("audit", "fail_closed"),
            ("off", "allow_unbounded"),
        ] {
            let toml = format!(
                r#"{prefix}

[lancet.isolation.command]
mode = "{mode}"
required_boundaries = ["network"]
unsupported_host = "{unsupported_host}"
"#,
                prefix = minimal_profile_prefix()
            );
            let err = parse(&toml).unwrap_err();
            assert!(
                err.to_string().contains("unsupported_host"),
                "should reject {mode}/{unsupported_host}, got: {err}"
            );
        }
    }

    #[test]
    fn cep_isolation_command_rejects_empty_or_traversing_paths() {
        let empty_toolchain = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "audit"
read_only_toolchain_paths = [""]
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&empty_toolchain).unwrap_err();
        assert!(
            err.to_string().contains("read_only_toolchain_paths"),
            "should reject empty toolchain path, got: {err}"
        );

        let traversing_scratch = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "audit"
scratch_paths = ["../tmp"]
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&traversing_scratch).unwrap_err();
        assert!(
            err.to_string().contains("scratch_paths"),
            "should reject traversing scratch path, got: {err}"
        );
    }

    #[test]
    fn cep_isolation_command_moved_from_extensions_to_typed() {
        let toml = format!(
            r#"{prefix}

[lancet.isolation.command]
mode = "audit"
required_boundaries = ["network"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert!(profile.lancet.isolation.is_some());
        assert!(!profile.lancet.extensions.contains_key("isolation"));
    }

    #[test]
    fn cep_serializes_extensions_flattened() {
        let mut future = toml::map::Map::new();
        future.insert("some_field".into(), toml::Value::Boolean(true));

        let mut extensions = BTreeMap::new();
        extensions.insert("future_table".into(), toml::Value::Table(future));

        let section = LancetSection {
            mode: CepMode::Warn,
            project_root: None,
            observation: None,
            network: None,
            command: None,
            policy: None,
            isolation: None,
            effects: None,
            workspace: None,
            verify: None,
            extensions,
        };
        let serialized = toml::to_string(&section).unwrap();
        assert!(serialized.contains("mode = \"warn\""), "got: {serialized}");
        assert!(serialized.contains("[future_table]"), "got: {serialized}");
        assert!(!serialized.contains("extensions"), "got: {serialized}");
    }

    // ── Tools section defaults ────────────────────────────────────────

    #[test]
    fn tools_section_defaults_to_empty_enabled() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"
"#;
        let profile = parse(toml).unwrap();
        assert!(profile.tools.enabled.is_empty());
    }

    #[test]
    fn llm_retry_defaults_when_section_absent() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert_eq!(profile.llm.retry.max_retries, 3);
        assert_eq!(profile.llm.retry.initial_delay_ms, 500);
        assert_eq!(profile.llm.retry.max_delay_ms, 3_000);
    }

    #[test]
    fn llm_retry_parses_explicit_values() {
        let toml = format!(
            r#"{prefix}

[llm.retry]
max_retries = 0
initial_delay_ms = 1
max_delay_ms = 2
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.llm.retry.max_retries, 0);
        assert_eq!(profile.llm.retry.initial_delay_ms, 1);
        assert_eq!(profile.llm.retry.max_delay_ms, 2);
    }

    // ── WorkspaceSection / EgressPolicyConfig / CredentialRef ─────────────────

    #[test]
    fn workspace_section_missing_parses() {
        // Older profiles without [workspace] still parse; workspace field is None.
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(
            profile.workspace.is_none(),
            "workspace must be None when [workspace] is absent"
        );
    }

    #[test]
    fn workspace_egress_blocked_parses() {
        // [workspace] egress = "blocked" deserializes to EgressPolicyConfig::Blocked.
        let toml = format!(
            r#"{prefix}

[workspace]
egress = "blocked"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let ws = profile.workspace.as_ref().unwrap();
        assert_eq!(ws.egress, Some(EgressPolicyConfig::Blocked));
        assert!(ws.credentials.is_empty());
        assert!(ws.allowed_egress_targets.is_empty());
    }

    #[test]
    fn workspace_egress_allowlist_with_targets() {
        // egress = "allowlist" + allowed_egress_targets list parses correctly.
        let toml = format!(
            r#"{prefix}

[workspace]
egress = "allowlist"
allowed_egress_targets = ["api.openai.com", "pypi.org"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let ws = profile.workspace.as_ref().unwrap();
        assert_eq!(ws.egress, Some(EgressPolicyConfig::Allowlist));
        assert_eq!(
            ws.allowed_egress_targets,
            vec!["api.openai.com", "pypi.org"]
        );
    }

    #[test]
    fn workspace_credentials_name_only() {
        // credentials = [{ env_var_name = "OPENAI_API_KEY" }] parses;
        // CredentialRef has no value field — compile-time structural guarantee.
        let toml = format!(
            r#"{prefix}

[[workspace.credentials]]
env_var_name = "OPENAI_API_KEY"

[[workspace.credentials]]
env_var_name = "ANTHROPIC_API_KEY"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let ws = profile.workspace.as_ref().unwrap();
        assert_eq!(ws.credentials.len(), 2);
        assert_eq!(ws.credentials[0].env_var_name, "OPENAI_API_KEY");
        assert_eq!(ws.credentials[1].env_var_name, "ANTHROPIC_API_KEY");

        // Structural check: CredentialRef has only env_var_name; no value field exists.
        // Serializing must not produce any secret value in output.
        let serialized = toml::to_string(&profile).unwrap();
        let fake_secret = "sk-super-secret-value";
        assert!(
            !serialized.contains(fake_secret),
            "CredentialRef serialized form must not contain secret values; got: {serialized}"
        );
    }

    #[test]
    fn workspace_unknown_field_rejected() {
        // Extra key in [workspace] fails deny_unknown_fields.
        let toml = format!(
            r#"{prefix}

[workspace]
egress = "open"
unexpected_key = "bad"
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("unexpected_key") || err.to_string().contains("unknown field"),
            "unknown workspace field must be rejected, got: {err}"
        );
    }

    #[test]
    fn lancet_workspace_roundtrip() {
        let toml = format!(
            r#"{prefix}

[lancet.workspace]
allowed_write_paths = ["src/**", "crates/foo/**"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile: AgentProfile = parse(&toml).unwrap();
        let paths = &profile
            .lancet
            .workspace
            .as_ref()
            .unwrap()
            .allowed_write_paths;
        assert_eq!(
            paths,
            &vec!["src/**".to_string(), "crates/foo/**".to_string()]
        );
    }

    #[test]
    fn lancet_workspace_unknown_field_rejected() {
        let toml = format!(
            r#"{prefix}

[lancet.workspace]
allowed_write_paths = ["src/**"]
unexpected = true
"#,
            prefix = minimal_profile_prefix()
        );
        assert!(parse(&toml).is_err());
    }

    #[test]
    fn lancet_verify_roundtrip() {
        let toml = format!(
            r#"{prefix}

[lancet.verify]
argv = ["cargo", "test", "-p", "lattice-api"]
cwd = "."
expected_exit_code = 0
timeout_ms = 120000
"#,
            prefix = minimal_profile_prefix()
        );
        let profile: AgentProfile = parse(&toml).unwrap();
        let verify = profile.lancet.verify.as_ref().unwrap();
        assert_eq!(verify.argv, vec!["cargo", "test", "-p", "lattice-api"]);
        assert_eq!(verify.cwd.as_deref(), Some("."));
        assert_eq!(verify.expected_exit_code, Some(0));
        assert_eq!(verify.timeout_ms, Some(120_000));
    }

    #[test]
    fn workspace_image_optional_without_provisioner_default() {
        let toml = format!(
            r#"{prefix}

[workspace]
egress = "open"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile: AgentProfile = parse(&toml).unwrap();
        assert!(profile.workspace.as_ref().unwrap().image.is_none());
        let docs = include_str!("schema.rs");
        assert!(docs.contains("no provisioner default is implied by the profile schema"));
    }

    #[test]
    fn workspace_roundtrip() {
        // Serialize AgentProfile with workspace section and deserialize back equals.
        let toml_input = format!(
            r#"{prefix}

[workspace]
egress = "allowlist"
image = "docker.io/library/python:3.12-slim"
cpus = 2
memory_mb = 1024
allowed_egress_targets = ["pypi.org", "files.pythonhosted.org"]

[[workspace.credentials]]
env_var_name = "PYPI_TOKEN"
"#,
            prefix = minimal_profile_prefix()
        );

        let profile: AgentProfile = parse(&toml_input).unwrap();
        let ws = profile.workspace.as_ref().unwrap();
        assert_eq!(ws.egress, Some(EgressPolicyConfig::Allowlist));
        assert_eq!(
            ws.image.as_deref(),
            Some("docker.io/library/python:3.12-slim")
        );
        assert_eq!(ws.cpus, Some(2));
        assert_eq!(ws.memory_mb, Some(1024));
        assert_eq!(
            ws.allowed_egress_targets,
            vec!["pypi.org", "files.pythonhosted.org"]
        );
        assert_eq!(ws.credentials[0].env_var_name, "PYPI_TOKEN");

        // Roundtrip: serialize and parse back; result must equal original.
        let serialized = toml::to_string(&profile).unwrap();
        let profile2: AgentProfile = toml::from_str(&serialized).unwrap();
        assert_eq!(profile, profile2, "workspace roundtrip must be lossless");
    }

    /// Guard: no legacy symbols in production source.
    #[test]
    fn source_free_of_forbidden_symbols() {
        let source = include_str!("schema.rs");
        let test_start = source.find("#[cfg(test)]").unwrap_or(source.len());
        let prod = &source[..test_start];
        let forbidden_patterns = [
            "preset",
            "skippable",
            "tags",
            "behavior_type",
            "confidence_threshold",
            "enforce_sandbox",
            "code_agent",
            "code-agent",
            "AgentPreset",
            "ToolPreset",
            "SandboxMode",
            "SecuritySection",
            "ContextSection",
            "BehaviorSection",
        ];
        // Note: "bundles" is allowed in doc comments explaining its removal.
        for pattern in forbidden_patterns {
            if prod.contains(pattern) {
                panic!("schema.rs production code must not contain '{pattern}'");
            }
        }
    }

    // ── LancetPolicySection ────────────────────────────────────────────────

    #[test]
    fn cep_policy_section_parses_path() {
        let toml = format!(
            r#"{prefix}

[lancet.policy]
path = "policy.toml"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let policy = profile.lancet.policy.as_ref().unwrap();
        assert_eq!(policy.path.as_deref(), Some("policy.toml"));
    }

    #[test]
    fn cep_section_without_policy_defaults_to_none() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(profile.lancet.policy.is_none());
    }

    #[test]
    fn cep_policy_section_rejects_unknown_fields() {
        let toml = format!(
            r#"{prefix}

[lancet.policy]
unknown = true
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("unknown"),
            "should reject unknown field, got: {err}"
        );
    }

    // ── Workspace lifecycle limits ──────────────────────────────────────────

    #[test]
    fn workspace_lifecycle_limits_defaults_when_omitted() {
        let toml = format!(
            r#"{prefix}

[workspace]
egress = "open"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let ws = profile.workspace.as_ref().unwrap();
        assert_eq!(ws.cleanup_rounds, 3, "default cleanup_rounds must be 3");
        assert_eq!(
            ws.max_pending_snapshots, 5,
            "default max_pending_snapshots must be 5"
        );
    }

    #[test]
    fn workspace_lifecycle_limits_explicit_values_roundtrip() {
        let toml = format!(
            r#"{prefix}

[workspace]
cleanup_rounds = 10
max_pending_snapshots = 20
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let ws = profile.workspace.as_ref().unwrap();
        assert_eq!(ws.cleanup_rounds, 10);
        assert_eq!(ws.max_pending_snapshots, 20);

        // Roundtrip
        let serialized = toml::to_string(&profile).unwrap();
        let profile2: AgentProfile = toml::from_str(&serialized).unwrap();
        assert_eq!(
            profile, profile2,
            "lifecycle limits roundtrip must be lossless"
        );
    }

    #[test]
    fn workspace_lifecycle_limits_boundary_values_accepted() {
        let toml = format!(
            r#"{prefix}

[workspace]
cleanup_rounds = 1
max_pending_snapshots = 1
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let ws = profile.workspace.as_ref().unwrap();
        assert_eq!(ws.cleanup_rounds, 1);
        assert_eq!(ws.max_pending_snapshots, 1);

        let toml_upper = format!(
            r#"{prefix}

[workspace]
cleanup_rounds = 32
max_pending_snapshots = 100
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml_upper).unwrap();
        let ws = profile.workspace.as_ref().unwrap();
        assert_eq!(ws.cleanup_rounds, 32);
        assert_eq!(ws.max_pending_snapshots, 100);
    }

    #[test]
    fn workspace_lifecycle_limits_zero_rejected() {
        let toml = format!(
            r#"{prefix}

[workspace]
cleanup_rounds = 0
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("cleanup_rounds"),
            "zero cleanup_rounds must be rejected, got: {err}"
        );

        let toml2 = format!(
            r#"{prefix}

[workspace]
max_pending_snapshots = 0
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml2).unwrap_err();
        assert!(
            err.to_string().contains("max_pending_snapshots"),
            "zero max_pending_snapshots must be rejected, got: {err}"
        );
    }

    #[test]
    fn workspace_lifecycle_limits_out_of_range_rejected() {
        let toml = format!(
            r#"{prefix}

[workspace]
cleanup_rounds = 33
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("cleanup_rounds"),
            "cleanup_rounds=33 must be rejected, got: {err}"
        );

        let toml2 = format!(
            r#"{prefix}

[workspace]
max_pending_snapshots = 101
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml2).unwrap_err();
        assert!(
            err.to_string().contains("max_pending_snapshots"),
            "max_pending_snapshots=101 must be rejected, got: {err}"
        );
    }

    // ── Workspace compatibility and strictness ──────────────────────────────

    #[test]
    fn workspace_typo_cleanup_round_rejected() {
        // `cleanup_round` (singular) is a typo — must be `cleanup_rounds`
        let toml = format!(
            r#"{prefix}

[workspace]
cleanup_round = 5
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("cleanup_round") || err.to_string().contains("unknown field"),
            "typo `cleanup_round` must be rejected, got: {err}"
        );
    }

    #[test]
    fn workspace_typo_max_pending_snapshot_rejected() {
        // `max_pending_snapshot` (singular) is a typo — must be `max_pending_snapshots`
        let toml = format!(
            r#"{prefix}

[workspace]
max_pending_snapshot = 5
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("max_pending_snapshot")
                || err.to_string().contains("unknown field"),
            "typo `max_pending_snapshot` must be rejected, got: {err}"
        );
    }

    #[test]
    fn workspace_serialization_contains_resolved_lifecycle_values() {
        // A profile with [workspace] and omitted limits must serialize with
        // the resolved default values — ensuring downstream consumers see
        // explicit numbers in the canonical profile snapshot.
        let toml = format!(
            r#"{prefix}

[workspace]
egress = "open"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let serialized = toml::to_string(&profile).unwrap();
        assert!(
            serialized.contains("cleanup_rounds = 3"),
            "serialized profile must contain resolved cleanup_rounds, got: {serialized}"
        );
        assert!(
            serialized.contains("max_pending_snapshots = 5"),
            "serialized profile must contain resolved max_pending_snapshots, got: {serialized}"
        );
    }

    #[test]
    fn workspace_serialization_contains_no_secret_values() {
        // Even with credentials and lifecycle limits, no secret values appear
        // in the serialized profile.
        let toml = format!(
            r#"{prefix}

[workspace]
egress = "allowlist"
cleanup_rounds = 10
max_pending_snapshots = 20
allowed_egress_targets = ["api.example.com"]

[[workspace.credentials]]
env_var_name = "SUPER_SECRET_KEY"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let serialized = toml::to_string(&profile).unwrap();

        // Must contain lifecycle limits
        assert!(
            serialized.contains("cleanup_rounds = 10"),
            "must contain explicit cleanup_rounds"
        );
        assert!(
            serialized.contains("max_pending_snapshots = 20"),
            "must contain explicit max_pending_snapshots"
        );

        // Must NOT contain any secret value (only the env var name is stored)
        assert!(
            !serialized.contains("sk-") && !serialized.contains("secret-value"),
            "serialized profile must not contain secret values"
        );

        // env_var_name is present (it's a reference, not a secret)
        assert!(
            serialized.contains("SUPER_SECRET_KEY"),
            "CredentialRef env_var_name must be present in serialization"
        );
    }

    #[test]
    fn workspace_absent_section_parses_with_none() {
        // Profiles without [workspace] still parse; lifecycle code must apply
        // limits only when an Upper exists.
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(
            profile.workspace.is_none(),
            "absent [workspace] must produce None, not a default WorkspaceSection"
        );
    }
}
