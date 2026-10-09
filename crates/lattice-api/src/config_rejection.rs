// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{fmt, path::PathBuf};

use lattice_profile::{LlmIntegerField, ProfileError};

/// 只承载审核过的配置拒绝类别；不提供字符串构造或反序列化入口，
/// 任意错误文本仍须经过原有的固定文本边界。
#[derive(Clone, Debug)]
pub struct ConfigRejection(pub(crate) Reason);

#[derive(Clone, Debug)]
pub(crate) enum Reason {
    AgentsDirectory {
        path: PathBuf,
        rule: AgentsDirectoryRule,
    },
    ProfileNotFound {
        agent: String,
        project: PathBuf,
        global: PathBuf,
    },
    Toml {
        path: PathBuf,
        line: Option<std::num::NonZeroU32>,
    },
    SchemaVersion {
        path: PathBuf,
        found: u64,
    },
    Integer {
        path: PathBuf,
        field: LlmIntegerField,
        value: u64,
        min: u64,
        max: u64,
    },
    ProfileRule {
        path: PathBuf,
        rule: ProfileRule,
    },
    MissingDefault {
        dir: PathBuf,
    },
    AgentName {
        path: PathBuf,
        selected: String,
    },
    MissingApiKey {
        env_var: String,
    },
    EmptyApiKey {
        env_var: String,
    },
    BaseUrl(lattice_llm::BaseUrlRule),
    UnknownModel,
    UnknownTools {
        positions: Vec<usize>,
        available: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum AgentsDirectoryRule {
    System(&'static str),
    Temporary,
    Traversal,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ProfileRule {
    ForbiddenSection,
    ForbiddenField,
    RetiredObservationRoots,
    PromptMutuallyExclusive,
    MissingPrompt,
    PathEscape,
    AbsoluteSystemPath,
    SystemPathTraversal,
    SystemPathNotAFile,
    SystemPathNotFound,
}

impl ConfigRejection {
    pub(crate) fn profile(error: ProfileError, selected: &str) -> crate::ApiError {
        let reason = match error {
            ProfileError::Toml { path, line, .. } => Reason::Toml { path, line },
            ProfileError::UnsupportedSchemaVersion { path, found } => {
                Reason::SchemaVersion { path, found }
            }
            ProfileError::InvalidInteger {
                path,
                field,
                value,
                min,
                max,
            } => Reason::Integer {
                path,
                field,
                value,
                min,
                max,
            },
            ProfileError::ProfileNotFound {
                project_path,
                global_path,
                ..
            } => Reason::ProfileNotFound {
                agent: selected.into(),
                project: project_path,
                global: global_path,
            },
            ProfileError::MissingDefault { dir, .. } => Reason::MissingDefault { dir },
            ProfileError::ForbiddenSection { path, .. } => Reason::ProfileRule {
                path,
                rule: ProfileRule::ForbiddenSection,
            },
            ProfileError::ForbiddenField {
                path,
                section,
                field,
                ..
            } => Reason::ProfileRule {
                path,
                rule: if section == "lancet.observation" && field == "allowed_roots" {
                    ProfileRule::RetiredObservationRoots
                } else {
                    ProfileRule::ForbiddenField
                },
            },
            ProfileError::PromptMutuallyExclusive { path } => Reason::ProfileRule {
                path,
                rule: ProfileRule::PromptMutuallyExclusive,
            },
            ProfileError::MissingPrompt { path } => Reason::ProfileRule {
                path,
                rule: ProfileRule::MissingPrompt,
            },
            ProfileError::PathEscape { profile_path, .. } => Reason::ProfileRule {
                path: profile_path,
                rule: ProfileRule::PathEscape,
            },
            ProfileError::AbsoluteSystemPath { profile_path, .. } => Reason::ProfileRule {
                path: profile_path,
                rule: ProfileRule::AbsoluteSystemPath,
            },
            ProfileError::SystemPathTraversal { profile_path, .. } => Reason::ProfileRule {
                path: profile_path,
                rule: ProfileRule::SystemPathTraversal,
            },
            ProfileError::SystemPathNotAFile { profile_path, .. } => Reason::ProfileRule {
                path: profile_path,
                rule: ProfileRule::SystemPathNotAFile,
            },
            ProfileError::SystemPathNotFound { profile_path, .. } => Reason::ProfileRule {
                path: profile_path,
                rule: ProfileRule::SystemPathNotFound,
            },
            // Io 的路径可能来自 prompt.system_path；InvalidField 含任意字符串，
            // 不能享有类型化整数校验的展示权限。
            ProfileError::Io { .. } | ProfileError::InvalidField { .. } => {
                return crate::ApiError::BadRequest("invalid profile".into());
            }
        };
        crate::ApiError::Configuration(Self(reason))
    }

    pub(crate) fn llm(error: lattice_llm::LlmError) -> Option<Self> {
        use lattice_llm::LlmError;
        Some(Self(match error {
            LlmError::UnknownModel(_) => Reason::UnknownModel,
            LlmError::MissingApiKey { env_var } => Reason::MissingApiKey { env_var },
            LlmError::EmptyApiKey { env_var } => Reason::EmptyApiKey { env_var },
            LlmError::BaseUrl(rule) => Reason::BaseUrl(rule),
            LlmError::Credential { .. }
            | LlmError::Config { .. }
            | LlmError::Dns { .. }
            | LlmError::Transport { .. }
            | LlmError::Timeout { .. }
            | LlmError::ProviderHttp { .. }
            | LlmError::StreamRead { .. }
            | LlmError::ProtocolLimit { .. }
            | LlmError::Projection(_)
            | LlmError::Other(_) => return None,
        }))
    }

    /// `available` 必须来自构建期封闭的 Component 目录，不能来自 profile。
    pub(crate) fn unknown_tools(enabled: &[String], available: Vec<String>) -> Self {
        let positions = enabled
            .iter()
            .enumerate()
            .filter(|(_, name)| !available.contains(name))
            .map(|(index, _)| index + 1)
            .collect();
        Self(Reason::UnknownTools {
            positions,
            available,
        })
    }
}

impl fmt::Display for ConfigRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match &self.0 {
            Reason::AgentsDirectory { path, rule } => {
                let detail = match rule {
                    AgentsDirectoryRule::System(prefix) => format!("is inside a system path ({prefix}). Set LATTICE_AGENTS_DIR to a safe location (e.g., under $HOME or your project)."),
                    AgentsDirectoryRule::Temporary => "is inside /tmp which is world-writable and unsafe. Set LATTICE_AGENTS_DIR to a safe location.".into(),
                    AgentsDirectoryRule::Traversal => "contains path traversal. Set LATTICE_AGENTS_DIR to a safe location.".into(),
                };
                format!("Agents directory '{}' {detail}", path.display())
            }
            Reason::ProfileNotFound {
                agent,
                project,
                global,
            } => format!(
                "agent profile '{agent}' not found at project path {} or global path {}",
                project.display(),
                global.display()
            ),
            Reason::Toml { path, line } => match line {
                Some(line) => format!("invalid profile TOML at {}: line {line}", path.display()),
                None => format!("invalid profile TOML at {}", path.display()),
            },
            Reason::SchemaVersion { path, found } => format!(
                "invalid schema_version {found} at {}: only schema_version = 1 is supported",
                path.display()
            ),
            Reason::Integer {
                path,
                field,
                value,
                min,
                max,
            } => format!(
                "invalid value {value} for {field} at {}: expected {min}..={max}",
                path.display()
            ),
            Reason::ProfileRule { path, rule } => {
                let detail = match rule {
                    ProfileRule::ForbiddenSection => "forbidden or incorrectly spelled section",
                    ProfileRule::ForbiddenField => "forbidden or retired field",
                    ProfileRule::RetiredObservationRoots => {
                        "retired field [lancet.observation].allowed_roots; use [lancet.project_root].root for scope and [lancet.workspace] for read visibility"
                    }
                    ProfileRule::PromptMutuallyExclusive => {
                        "both `system` and `system_path` set in [prompt]: exactly one is required"
                    }
                    ProfileRule::MissingPrompt => {
                        "neither `system` nor `system_path` set in [prompt]: exactly one is required"
                    }
                    ProfileRule::PathEscape => "[prompt].system_path escapes package directory",
                    ProfileRule::AbsoluteSystemPath => {
                        "[prompt].system_path must be relative to the agent.toml package directory"
                    }
                    ProfileRule::SystemPathTraversal => {
                        "[prompt].system_path contains '..': traversal is not allowed"
                    }
                    ProfileRule::SystemPathNotAFile => "[prompt].system_path is not a file",
                    ProfileRule::SystemPathNotFound => "[prompt].system_path could not be resolved",
                };
                format!("{detail} at {}", path.display())
            }
            Reason::MissingDefault { dir } => format!(
                "no default agent profile found: create {}/agent.toml, or specify an agent with `--agent <name>`",
                dir.display()
            ),
            Reason::AgentName { path, selected } => format!(
                "[agent].name at {} must match selected agent '{selected}'",
                path.display()
            ),
            Reason::MissingApiKey { env_var } => {
                format!("missing API key: environment variable '{env_var}' is not set")
            }
            Reason::EmptyApiKey { env_var } => {
                format!("missing API key: environment variable '{env_var}' is empty")
            }
            Reason::BaseUrl(rule) => rule.to_string(),
            Reason::UnknownModel => {
                "[agent].model: unknown model alias; use `lattice models` to list known aliases"
                    .into()
            }
            Reason::UnknownTools {
                positions,
                available,
            } => format!(
                "[tools].enabled contains unknown tools at items {positions:?} (1-based); available catalog tools: {}",
                available.join(", ")
            ),
        };
        f.write_str(&bound_reason(&text))
    }
}

// 沿用 lattice_llm::bound_diagnostic 的单行语义；该函数固定限长 256，
// 此处为两条查找路径和封闭工具目录保留更多空间。
const MAX_REASON_CHARS: usize = 1024;

fn bound_reason(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX_REASON_CHARS {
        line
    } else {
        line.chars()
            .take(MAX_REASON_CHARS - 1)
            .chain(['…'])
            .collect()
    }
}

#[cfg(test)]
#[path = "config_rejection_tests.rs"]
mod tests;
