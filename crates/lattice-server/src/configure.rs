// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 会话设置的纯函数计算，唯一执行 ADR 0012 第 2 节的档位选择与回退规则。

use std::path::PathBuf;

use crate::{ConfigRejection, Effort, ModelRef, local::EffortAdjustment, profile::TurnProfile};
use lattice_llm::{EffortPolicy, PrepareError};
use lattice_session::store::SessionSettings;

/// 已加载的 agent 快照与源文件路径，供配置诊断写明档位来源。
pub(crate) struct LoadedAgent {
    pub profile: TurnProfile,
    pub source_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EffortSource {
    Explicit,
    ProfileFile(PathBuf),
    SessionSettings,
}

impl EffortSource {
    pub(crate) fn format_unsupported(&self, error: &PrepareError) -> String {
        match error {
            PrepareError::UnsupportedEffort { .. } => match self {
                Self::Explicit => format!("{error} (from --effort)"),
                Self::ProfileFile(path) => {
                    format!("{error} (from [agent].effort in {})", path.display())
                }
                Self::SessionSettings => format!("{error} (from session settings)"),
            },
            _ => error.to_string(),
        }
    }
}

/// `resolve_settings` 的计算基准：显式给定的 agent profile，或已有会话的当前设置。
pub(crate) enum SettingsBase<'a> {
    Agent(LoadedAgent),
    Current(&'a SessionSettings),
}

/// 纯函数计算新的会话设置与档位调整；不访问文件系统、会话库或网络。
pub(crate) fn resolve_settings(
    base: SettingsBase<'_>,
    cmd_model: Option<ModelRef>,
    cmd_effort: Option<Effort>,
    supported_efforts: impl FnOnce(&ModelRef) -> Result<Vec<Effort>, lattice_llm::LlmError>,
) -> Result<(SessionSettings, Option<EffortAdjustment>), ConfigRejection> {
    let (profile, system_prompt, base_model, base_effort, default_effort, same_model_source) =
        match base {
            SettingsBase::Agent(loaded) => {
                let base_model = loaded.profile.effective.default_model.clone();
                let base_effort = loaded.profile.effective.default_effort;
                (
                    loaded.profile.effective,
                    loaded.profile.system_prompt,
                    base_model,
                    base_effort,
                    base_effort,
                    EffortSource::ProfileFile(loaded.source_path),
                )
            }
            SettingsBase::Current(current) => (
                current.profile.clone(),
                current.system_prompt.clone(),
                current.model.clone(),
                current.effort,
                current.profile.default_effort,
                EffortSource::SessionSettings,
            ),
        };

    let model = cmd_model.or_else(|| base_model.clone());
    let (effort, adjustment) = match model.as_ref() {
        None => {
            if cmd_effort.is_some() {
                return Err(ConfigRejection::new(
                    "cannot set effort without a model; choose one with /model or --model",
                    None,
                ));
            }
            (base_effort, None)
        }
        Some(chosen_model) => {
            let supported = supported_efforts(chosen_model)
                .map_err(|error| ConfigRejection::new(&error.to_string(), None))?;
            if let Some(explicit) = cmd_effort {
                let selected =
                    lattice_llm::select_effort(&EffortPolicy::Strict(Some(explicit)), &supported)
                        .map_err(|effort| {
                        let message = EffortSource::Explicit.format_unsupported(
                            &PrepareError::UnsupportedEffort {
                                model: chosen_model.clone(),
                                effort,
                            },
                        );
                        ConfigRejection::new(&message, None)
                    })?;
                (selected, None)
            } else if base_model.as_ref() == Some(chosen_model) {
                let selected =
                    lattice_llm::select_effort(&EffortPolicy::Strict(base_effort), &supported)
                        .map_err(|effort| {
                            let message = same_model_source.format_unsupported(
                                &PrepareError::UnsupportedEffort {
                                    model: chosen_model.clone(),
                                    effort,
                                },
                            );
                            ConfigRejection::new(&message, None)
                        })?;
                (selected, None)
            } else {
                let selected = lattice_llm::select_effort(
                    &EffortPolicy::Fallback {
                        current: base_effort,
                        profile_default: default_effort,
                    },
                    &supported,
                )
                .expect("Fallback policy never returns Err");
                let adjustment = (selected != base_effort).then_some(EffortAdjustment {
                    from: base_effort,
                    to: selected,
                });
                (selected, adjustment)
            }
        }
    };

    Ok((
        SessionSettings {
            profile,
            system_prompt,
            model,
            effort,
        },
        adjustment,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_conversation::Effort::{High, Low, Max, Medium, Off};
    use lattice_session::{
        AgentName, RetryPolicy, TurnTimeout,
        store::{EffectiveProfile, SessionSettings},
    };

    fn model_ref(text: &str) -> ModelRef {
        ModelRef::parse(text).expect("解析测试 ModelRef")
    }

    fn effective(
        name: &str,
        default_model: Option<&str>,
        default_effort: Option<Effort>,
    ) -> EffectiveProfile {
        EffectiveProfile {
            agent_name: AgentName::parse(name).expect("解析测试 AgentName"),
            description: Some(format!("{name} 描述")),
            default_model: default_model.map(model_ref),
            default_effort,
            timeout_secs: TurnTimeout::default(),
            retry: RetryPolicy::default(),
        }
    }

    fn loaded_agent(
        name: &str,
        default_model: Option<&str>,
        default_effort: Option<Effort>,
        prompt: &str,
        path: &str,
    ) -> LoadedAgent {
        LoadedAgent {
            profile: TurnProfile {
                effective: effective(name, default_model, default_effort),
                system_prompt: Some(prompt.to_owned()),
            },
            source_path: PathBuf::from(path),
        }
    }

    #[test]
    fn resolve_settings_covers_adr_0012_effort_branches_and_sources() {
        let existing = SessionSettings {
            profile: effective("alpha", Some("local/default"), Some(Low)),
            system_prompt: Some("alpha prompt".into()),
            model: Some(model_ref("local/current")),
            effort: Some(High),
        };
        let existing_no_effort = SessionSettings {
            effort: None,
            ..existing.clone()
        };
        let existing_no_model = SessionSettings {
            model: None,
            effort: None,
            ..existing.clone()
        };
        let agent_path = "/workspace/.lattice/agents/coder/agent.toml";

        struct SuccessCase<'a> {
            label: &'static str,
            base: SettingsBase<'a>,
            model: Option<&'static str>,
            effort: Option<Effort>,
            supported: &'static [Effort],
            expect_query: Option<&'static str>,
            expected_agent: &'static str,
            expected_prompt: Option<&'static str>,
            expected_model: Option<&'static str>,
            expected_effort: Option<Effort>,
            expected_adjustment: Option<EffortAdjustment>,
        }

        let cases = [
            SuccessCase {
                label: "新建会话给出 agent：沿用 [agent].model 与受支持的 [agent].effort",
                base: SettingsBase::Agent(loaded_agent(
                    "coder",
                    Some("local/sonnet"),
                    Some(Medium),
                    "coder prompt",
                    agent_path,
                )),
                model: None,
                effort: None,
                supported: &[Low, Medium, High],
                expect_query: Some("local/sonnet"),
                expected_agent: "coder",
                expected_prompt: Some("coder prompt"),
                expected_model: Some("local/sonnet"),
                expected_effort: Some(Medium),
                expected_adjustment: None,
            },
            SuccessCase {
                label: "给出 agent 同时换模型：当前基准不支持且默认也不支持时调整为空",
                base: SettingsBase::Agent(loaded_agent(
                    "coder",
                    Some("local/sonnet"),
                    Some(High),
                    "coder prompt",
                    agent_path,
                )),
                model: Some("local/plain"),
                effort: None,
                supported: &[],
                expect_query: Some("local/plain"),
                expected_agent: "coder",
                expected_prompt: Some("coder prompt"),
                expected_model: Some("local/plain"),
                expected_effort: None,
                expected_adjustment: Some(EffortAdjustment {
                    from: Some(High),
                    to: None,
                }),
            },
            SuccessCase {
                label: "不给 agent 同模型：严格保留当前档位，无调整",
                base: SettingsBase::Current(&existing),
                model: Some("local/current"),
                effort: None,
                supported: &[Low, High],
                expect_query: Some("local/current"),
                expected_agent: "alpha",
                expected_prompt: Some("alpha prompt"),
                expected_model: Some("local/current"),
                expected_effort: Some(High),
                expected_adjustment: None,
            },
            SuccessCase {
                label: "换模型且新模型支持原档位：保留原档位，无调整",
                base: SettingsBase::Current(&existing),
                model: Some("other/both"),
                effort: None,
                supported: &[Low, High],
                expect_query: Some("other/both"),
                expected_agent: "alpha",
                expected_prompt: Some("alpha prompt"),
                expected_model: Some("other/both"),
                expected_effort: Some(High),
                expected_adjustment: None,
            },
            SuccessCase {
                label: "换模型且原档位不支持：回退到快照中的 [agent].effort 并报告调整",
                base: SettingsBase::Current(&existing),
                model: Some("other/low-only"),
                effort: None,
                supported: &[Low],
                expect_query: Some("other/low-only"),
                expected_agent: "alpha",
                expected_prompt: Some("alpha prompt"),
                expected_model: Some("other/low-only"),
                expected_effort: Some(Low),
                expected_adjustment: Some(EffortAdjustment {
                    from: Some(High),
                    to: Some(Low),
                }),
            },
            SuccessCase {
                label: "换模型且原档位为空、默认档位受支持：回退到默认档位并报告调整",
                base: SettingsBase::Current(&existing_no_effort),
                model: Some("other/low-only"),
                effort: None,
                supported: &[Low],
                expect_query: Some("other/low-only"),
                expected_agent: "alpha",
                expected_prompt: Some("alpha prompt"),
                expected_model: Some("other/low-only"),
                expected_effort: Some(Low),
                expected_adjustment: Some(EffortAdjustment {
                    from: None,
                    to: Some(Low),
                }),
            },
            SuccessCase {
                label: "显式给出受支持档位：即使与原档位不同也不计为自动调整",
                base: SettingsBase::Current(&existing),
                model: None,
                effort: Some(Low),
                supported: &[Low],
                expect_query: Some("local/current"),
                expected_agent: "alpha",
                expected_prompt: Some("alpha prompt"),
                expected_model: Some("local/current"),
                expected_effort: Some(Low),
                expected_adjustment: None,
            },
            SuccessCase {
                label: "模型为空且未显式给 effort：照搬基准档位且不查询模型能力",
                base: SettingsBase::Agent(loaded_agent(
                    "modeless",
                    None,
                    Some(Low),
                    "p",
                    agent_path,
                )),
                model: None,
                effort: None,
                supported: &[],
                expect_query: None,
                expected_agent: "modeless",
                expected_prompt: Some("p"),
                expected_model: None,
                expected_effort: Some(Low),
                expected_adjustment: None,
            },
        ];

        for case in cases {
            let mut queried = None;
            let (settings, adjustment) = resolve_settings(
                case.base,
                case.model.map(model_ref),
                case.effort,
                |target| {
                    queried = Some(target.to_string());
                    Ok(case.supported.to_vec())
                },
            )
            .unwrap_or_else(|err| panic!("{} 应成功，实际拒绝: {err}", case.label));
            assert_eq!(
                queried.as_deref(),
                case.expect_query,
                "{}: 模型能力查询目标不符",
                case.label
            );
            assert_eq!(
                settings.profile.agent_name.as_str(),
                case.expected_agent,
                "{}: agent 名不符",
                case.label
            );
            assert_eq!(
                settings.system_prompt.as_deref(),
                case.expected_prompt,
                "{}: 系统提示不符",
                case.label
            );
            assert_eq!(
                settings.model.as_ref().map(ToString::to_string).as_deref(),
                case.expected_model,
                "{}: 模型不符",
                case.label
            );
            assert_eq!(
                settings.effort, case.expected_effort,
                "{}: 档位不符",
                case.label
            );
            assert_eq!(
                adjustment, case.expected_adjustment,
                "{}: 档位调整说明不符",
                case.label
            );
        }

        struct ErrorCase<'a> {
            label: &'static str,
            base: SettingsBase<'a>,
            model: Option<&'static str>,
            effort: Option<Effort>,
            query_result: Result<Vec<Effort>, lattice_llm::LlmError>,
            expected_substring: &'static str,
            forbidden_substrings: &'static [&'static str],
        }

        let error_cases = [
            ErrorCase {
                label: "显式 --effort 不被模型支持时标明 (from --effort)",
                base: SettingsBase::Current(&existing),
                model: Some("other/low-only"),
                effort: Some(Max),
                query_result: Ok(vec![Low]),
                expected_substring: "(from --effort)",
                forbidden_substrings: &["[agent].effort", "session settings"],
            },
            ErrorCase {
                label: "给出 agent 且同模型不支持 [agent].effort 时标明 profile 路径",
                base: SettingsBase::Agent(loaded_agent(
                    "coder",
                    Some("local/sonnet"),
                    Some(Off),
                    "p",
                    agent_path,
                )),
                model: None,
                effort: None,
                query_result: Ok(vec![Low, High]),
                expected_substring: "(from [agent].effort in /workspace/.lattice/agents/coder/agent.toml)",
                forbidden_substrings: &["--effort", "session settings"],
            },
            ErrorCase {
                label: "沿用当前设置且同模型档位失效时标明 (from session settings)",
                base: SettingsBase::Current(&existing),
                model: None,
                effort: None,
                query_result: Ok(vec![Low]),
                expected_substring: "(from session settings)",
                forbidden_substrings: &["--effort", "[agent].effort"],
            },
            ErrorCase {
                label: "模型为空但显式给了 effort 时拒绝并提示先选择模型",
                base: SettingsBase::Current(&existing_no_model),
                model: None,
                effort: Some(High),
                query_result: Ok(vec![High]),
                expected_substring: "model",
                forbidden_substrings: &[],
            },
            ErrorCase {
                label: "档位能力查询报错时以配置拒绝返回",
                base: SettingsBase::Current(&existing),
                model: Some("missing/model"),
                effort: None,
                query_result: Err(lattice_llm::LlmError::Config {
                    message: "provider \"missing\" is not configured".into(),
                }),
                expected_substring: "provider \"missing\" is not configured",
                forbidden_substrings: &[],
            },
        ];

        for case in error_cases {
            let err = resolve_settings(case.base, case.model.map(model_ref), case.effort, |_| {
                case.query_result
            })
            .expect_err(&format!("{} 应被拒绝", case.label));
            let text = err.to_string();
            assert!(
                text.contains(case.expected_substring),
                "{}: 期望包含 {:?}，实际为 {text:?}",
                case.label,
                case.expected_substring
            );
            for forbidden in case.forbidden_substrings {
                assert!(
                    !text.contains(forbidden),
                    "{}: 不应包含 {:?}，实际为 {text:?}",
                    case.label,
                    forbidden
                );
            }
        }
    }
}
