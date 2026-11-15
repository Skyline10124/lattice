// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 后台边界的模型查询与模型/凭据展示类型逐字段转换。
//!
//! 所有枚举采用无通配符的穷尽 `match`，所有结构体先完整解构再逐字段构造，
//! 不经 JSON 往返，也不重新计算可用性、价格、档位或告警。

pub(super) fn model_query(
    query: &lattice_ipc::ModelCandidateQuery,
) -> lattice_llm::ModelCandidateQuery {
    match query {
        lattice_ipc::ModelCandidateQuery::List {
            provider,
            ready_only,
        } => lattice_llm::ModelCandidateQuery::List {
            provider: provider.clone(),
            ready_only: *ready_only,
        },
        lattice_ipc::ModelCandidateQuery::Target(target) => {
            lattice_llm::ModelCandidateQuery::Target(target.clone())
        }
    }
}

pub(super) fn model_candidates(
    candidates: lattice_llm::ModelCandidates,
) -> lattice_ipc::ModelCandidates {
    let lattice_llm::ModelCandidates { models, warnings } = candidates;
    lattice_ipc::ModelCandidates {
        models: models.into_iter().map(model_candidate).collect(),
        warnings: warnings.into_iter().map(model_warning).collect(),
    }
}

fn model_candidate(candidate: lattice_llm::ModelCandidate) -> lattice_ipc::ModelCandidate {
    let lattice_llm::ModelCandidate {
        provider,
        model,
        name,
        context,
        output,
        supported_efforts,
        cost,
        availability: status,
    } = candidate;
    lattice_ipc::ModelCandidate {
        provider,
        model,
        name,
        context,
        output,
        supported_efforts,
        cost: cost.map(model_price),
        availability: availability(status),
    }
}

fn model_price(price: lattice_llm::ModelPrice) -> lattice_ipc::ModelPrice {
    let lattice_llm::ModelPrice {
        input,
        output,
        cache_read,
        cache_write,
    } = price;
    lattice_ipc::ModelPrice {
        input,
        output,
        cache_read,
        cache_write,
    }
}

fn model_warning(warning: lattice_llm::ModelWarning) -> lattice_ipc::ModelWarning {
    let lattice_llm::ModelWarning { provider, message } = warning;
    lattice_ipc::ModelWarning { provider, message }
}

pub(super) fn availability(value: lattice_llm::Availability) -> lattice_ipc::Availability {
    match value {
        lattice_llm::Availability::Ready => lattice_ipc::Availability::Ready,
        lattice_llm::Availability::Unconfigured { requirement } => {
            lattice_ipc::Availability::Unconfigured {
                requirement: configuration_requirement(requirement),
            }
        }
        lattice_llm::Availability::Invalid { reason } => {
            lattice_ipc::Availability::Invalid { reason }
        }
    }
}

pub(super) fn configuration_requirement(
    value: lattice_llm::ConfigurationRequirement,
) -> lattice_ipc::ConfigurationRequirement {
    match value {
        lattice_llm::ConfigurationRequirement::Declaration => {
            lattice_ipc::ConfigurationRequirement::Declaration
        }
        lattice_llm::ConfigurationRequirement::ApiKey => {
            lattice_ipc::ConfigurationRequirement::ApiKey
        }
        lattice_llm::ConfigurationRequirement::ChatgptLogin => {
            lattice_ipc::ConfigurationRequirement::ChatgptLogin
        }
    }
}

pub(super) fn auth_status(status: lattice_llm::auth::AuthStatus) -> lattice_ipc::AuthStatus {
    let lattice_llm::auth::AuthStatus {
        providers_path,
        providers,
        error,
    } = status;
    let providers = providers
        .into_iter()
        .map(|provider| provider_auth_status(&providers_path, provider))
        .collect();
    lattice_ipc::AuthStatus {
        providers_path,
        providers,
        error,
    }
}

pub(super) fn provider_auth_status(
    providers_path: &std::path::Path,
    status: lattice_llm::auth::ProviderAuthStatus,
) -> lattice_ipc::ProviderAuthStatus {
    let lattice_llm::auth::ProviderAuthStatus {
        provider,
        availability: status_availability,
        email,
        expires_at,
    } = status;
    let diagnostic = status_availability.diagnostic(&provider, providers_path);
    lattice_ipc::ProviderAuthStatus {
        provider,
        availability: availability(status_availability),
        email,
        expires_at,
        diagnostic,
    }
}

pub(super) fn revocation_status(
    status: lattice_llm::auth::RevocationStatus,
) -> lattice_ipc::RevocationStatus {
    match status {
        lattice_llm::auth::RevocationStatus::Confirmed => lattice_ipc::RevocationStatus::Confirmed,
        lattice_llm::auth::RevocationStatus::Unconfirmed => {
            lattice_ipc::RevocationStatus::Unconfirmed
        }
        lattice_llm::auth::RevocationStatus::NotNeeded => lattice_ipc::RevocationStatus::NotNeeded,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lattice_conversation::{Effort, ModelId, ModelRef, ProviderId};

    use super::*;

    #[test]
    fn enum_conversions_cover_every_branch() {
        for (label, ipc_q, expected_llm_q) in [
            (
                "query/list_some",
                lattice_ipc::ModelCandidateQuery::List {
                    provider: Some(ProviderId::parse("openai").unwrap()),
                    ready_only: true,
                },
                lattice_llm::ModelCandidateQuery::List {
                    provider: Some(ProviderId::parse("openai").unwrap()),
                    ready_only: true,
                },
            ),
            (
                "query/list_none",
                lattice_ipc::ModelCandidateQuery::List {
                    provider: None,
                    ready_only: false,
                },
                lattice_llm::ModelCandidateQuery::List {
                    provider: None,
                    ready_only: false,
                },
            ),
            (
                "query/target",
                lattice_ipc::ModelCandidateQuery::Target(ModelRef::parse("openai/gpt-5").unwrap()),
                lattice_llm::ModelCandidateQuery::Target(ModelRef::parse("openai/gpt-5").unwrap()),
            ),
        ] {
            assert_eq!(
                model_query(&ipc_q),
                expected_llm_q,
                "{label} IPC 到 llm 查询转换结果不符"
            );
        }

        for (label, llm_req, expected_ipc_req) in [
            (
                "requirement/declaration",
                lattice_llm::ConfigurationRequirement::Declaration,
                lattice_ipc::ConfigurationRequirement::Declaration,
            ),
            (
                "requirement/api_key",
                lattice_llm::ConfigurationRequirement::ApiKey,
                lattice_ipc::ConfigurationRequirement::ApiKey,
            ),
            (
                "requirement/chatgpt_login",
                lattice_llm::ConfigurationRequirement::ChatgptLogin,
                lattice_ipc::ConfigurationRequirement::ChatgptLogin,
            ),
        ] {
            assert_eq!(
                configuration_requirement(llm_req),
                expected_ipc_req,
                "{label} llm 到 IPC 配置要求转换结果不符"
            );
        }

        for (label, llm_avail, expected_ipc_avail) in [
            (
                "availability/ready",
                lattice_llm::Availability::Ready,
                lattice_ipc::Availability::Ready,
            ),
            (
                "availability/unconfigured_declaration",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::Declaration,
                },
                lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::Declaration,
                },
            ),
            (
                "availability/unconfigured_api_key",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::ApiKey,
                },
                lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::ApiKey,
                },
            ),
            (
                "availability/unconfigured_chatgpt_login",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::ChatgptLogin,
                },
                lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::ChatgptLogin,
                },
            ),
            (
                "availability/invalid",
                lattice_llm::Availability::Invalid {
                    reason: "invalid base_url in providers.toml".to_owned(),
                },
                lattice_ipc::Availability::Invalid {
                    reason: "invalid base_url in providers.toml".to_owned(),
                },
            ),
        ] {
            assert_eq!(
                availability(llm_avail),
                expected_ipc_avail,
                "{label} llm 到 IPC 可用性转换结果不符"
            );
        }

        for (label, llm_rev, expected_ipc_rev) in [
            (
                "revocation/confirmed",
                lattice_llm::auth::RevocationStatus::Confirmed,
                lattice_ipc::RevocationStatus::Confirmed,
            ),
            (
                "revocation/unconfirmed",
                lattice_llm::auth::RevocationStatus::Unconfirmed,
                lattice_ipc::RevocationStatus::Unconfirmed,
            ),
            (
                "revocation/not_needed",
                lattice_llm::auth::RevocationStatus::NotNeeded,
                lattice_ipc::RevocationStatus::NotNeeded,
            ),
        ] {
            assert_eq!(
                revocation_status(llm_rev),
                expected_ipc_rev,
                "{label} 吊销状态转换结果不符"
            );
        }
    }

    #[test]
    fn struct_conversions_cover_full_and_minimal() {
        let price_full = lattice_llm::ModelPrice {
            input: Some(1.25),
            output: Some(10.0),
            cache_read: Some(0.125),
            cache_write: Some(1.5625),
        };
        let price_minimal = lattice_llm::ModelPrice {
            input: None,
            output: None,
            cache_read: None,
            cache_write: None,
        };
        assert_eq!(
            model_price(price_full.clone()),
            lattice_ipc::ModelPrice {
                input: Some(1.25),
                output: Some(10.0),
                cache_read: Some(0.125),
                cache_write: Some(1.5625),
            },
            "model_price/full 转换结果不符"
        );
        assert_eq!(
            model_price(price_minimal),
            lattice_ipc::ModelPrice {
                input: None,
                output: None,
                cache_read: None,
                cache_write: None,
            },
            "model_price/minimal 转换结果不符"
        );

        let candidate_full = lattice_llm::ModelCandidate {
            provider: ProviderId::parse("openai").unwrap(),
            model: ModelId::parse("gpt-5").unwrap(),
            name: Some("GPT-5".to_owned()),
            context: Some(400_000),
            output: Some(128_000),
            supported_efforts: vec![Effort::Low, Effort::Medium, Effort::High],
            cost: Some(price_full),
            availability: lattice_llm::Availability::Ready,
        };
        let candidate_minimal = lattice_llm::ModelCandidate {
            provider: ProviderId::parse("local").unwrap(),
            model: ModelId::parse("plain-v1").unwrap(),
            name: None,
            context: None,
            output: None,
            supported_efforts: Vec::new(),
            cost: None,
            availability: lattice_llm::Availability::Unconfigured {
                requirement: lattice_llm::ConfigurationRequirement::ApiKey,
            },
        };
        assert_eq!(
            model_candidate(candidate_full.clone()),
            lattice_ipc::ModelCandidate {
                provider: ProviderId::parse("openai").unwrap(),
                model: ModelId::parse("gpt-5").unwrap(),
                name: Some("GPT-5".to_owned()),
                context: Some(400_000),
                output: Some(128_000),
                supported_efforts: vec![Effort::Low, Effort::Medium, Effort::High],
                cost: Some(lattice_ipc::ModelPrice {
                    input: Some(1.25),
                    output: Some(10.0),
                    cache_read: Some(0.125),
                    cache_write: Some(1.5625),
                }),
                availability: lattice_ipc::Availability::Ready,
            },
            "model_candidate/full 转换结果不符"
        );
        assert_eq!(
            model_candidate(candidate_minimal.clone()),
            lattice_ipc::ModelCandidate {
                provider: ProviderId::parse("local").unwrap(),
                model: ModelId::parse("plain-v1").unwrap(),
                name: None,
                context: None,
                output: None,
                supported_efforts: Vec::new(),
                cost: None,
                availability: lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::ApiKey,
                },
            },
            "model_candidate/minimal 转换结果不符"
        );

        let warning_full = lattice_llm::ModelWarning {
            provider: Some("openai".to_owned()),
            message: "cached catalog is older than 7 days".to_owned(),
        };
        let warning_minimal = lattice_llm::ModelWarning {
            provider: None,
            message: "using built-in model metadata".to_owned(),
        };
        assert_eq!(
            model_warning(warning_full.clone()),
            lattice_ipc::ModelWarning {
                provider: Some("openai".to_owned()),
                message: "cached catalog is older than 7 days".to_owned(),
            },
            "model_warning/full 转换结果不符"
        );
        assert_eq!(
            model_warning(warning_minimal.clone()),
            lattice_ipc::ModelWarning {
                provider: None,
                message: "using built-in model metadata".to_owned(),
            },
            "model_warning/minimal 转换结果不符"
        );

        let candidates_full = lattice_llm::ModelCandidates {
            models: vec![candidate_full, candidate_minimal],
            warnings: vec![warning_full, warning_minimal],
        };
        let candidates_minimal = lattice_llm::ModelCandidates {
            models: Vec::new(),
            warnings: Vec::new(),
        };
        assert_eq!(
            model_candidates(candidates_full),
            lattice_ipc::ModelCandidates {
                models: vec![
                    lattice_ipc::ModelCandidate {
                        provider: ProviderId::parse("openai").unwrap(),
                        model: ModelId::parse("gpt-5").unwrap(),
                        name: Some("GPT-5".to_owned()),
                        context: Some(400_000),
                        output: Some(128_000),
                        supported_efforts: vec![Effort::Low, Effort::Medium, Effort::High],
                        cost: Some(lattice_ipc::ModelPrice {
                            input: Some(1.25),
                            output: Some(10.0),
                            cache_read: Some(0.125),
                            cache_write: Some(1.5625),
                        }),
                        availability: lattice_ipc::Availability::Ready,
                    },
                    lattice_ipc::ModelCandidate {
                        provider: ProviderId::parse("local").unwrap(),
                        model: ModelId::parse("plain-v1").unwrap(),
                        name: None,
                        context: None,
                        output: None,
                        supported_efforts: Vec::new(),
                        cost: None,
                        availability: lattice_ipc::Availability::Unconfigured {
                            requirement: lattice_ipc::ConfigurationRequirement::ApiKey,
                        },
                    },
                ],
                warnings: vec![
                    lattice_ipc::ModelWarning {
                        provider: Some("openai".to_owned()),
                        message: "cached catalog is older than 7 days".to_owned(),
                    },
                    lattice_ipc::ModelWarning {
                        provider: None,
                        message: "using built-in model metadata".to_owned(),
                    },
                ],
            },
            "model_candidates/full 转换结果不符"
        );
        assert_eq!(
            model_candidates(candidates_minimal),
            lattice_ipc::ModelCandidates {
                models: Vec::new(),
                warnings: Vec::new(),
            },
            "model_candidates/minimal 转换结果不符"
        );

        let providers_path = Path::new("/home/dev/.lattice/providers.toml");
        let provider_status_ready = lattice_llm::auth::ProviderAuthStatus {
            provider: "chatgpt".to_owned(),
            availability: lattice_llm::Availability::Ready,
            email: Some("dev@example.com".to_owned()),
            expires_at: Some("2026-06-01T00:00:00Z".to_owned()),
        };
        let provider_status_unconfigured = lattice_llm::auth::ProviderAuthStatus {
            provider: "openai".to_owned(),
            availability: lattice_llm::Availability::Unconfigured {
                requirement: lattice_llm::ConfigurationRequirement::ApiKey,
            },
            email: None,
            expires_at: None,
        };
        let provider_status_declaration = lattice_llm::auth::ProviderAuthStatus {
            provider: "lmstudio".to_owned(),
            availability: lattice_llm::Availability::Unconfigured {
                requirement: lattice_llm::ConfigurationRequirement::Declaration,
            },
            email: None,
            expires_at: None,
        };
        assert_eq!(
            provider_auth_status(providers_path, provider_status_ready.clone()),
            lattice_ipc::ProviderAuthStatus {
                provider: "chatgpt".to_owned(),
                availability: lattice_ipc::Availability::Ready,
                email: Some("dev@example.com".to_owned()),
                expires_at: Some("2026-06-01T00:00:00Z".to_owned()),
                diagnostic: None,
            },
            "provider_auth_status/ready 转换结果不符"
        );
        assert_eq!(
            provider_auth_status(providers_path, provider_status_unconfigured.clone()),
            lattice_ipc::ProviderAuthStatus {
                provider: "openai".to_owned(),
                availability: lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::ApiKey,
                },
                email: None,
                expires_at: None,
                diagnostic: Some(
                    "provider \"openai\" has no API key configured; run lattice auth set openai"
                        .to_owned()
                ),
            },
            "provider_auth_status/unconfigured_api_key 转换结果不符"
        );
        assert_eq!(
            provider_auth_status(providers_path, provider_status_declaration.clone()),
            lattice_ipc::ProviderAuthStatus {
                provider: "lmstudio".to_owned(),
                availability: lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::Declaration,
                },
                email: None,
                expires_at: None,
                diagnostic: Some(
                    "provider \"lmstudio\" is not declared; add [providers.lmstudio] in /home/dev/.lattice/providers.toml"
                        .to_owned()
                ),
            },
            "provider_auth_status/unconfigured_declaration 转换结果不符"
        );

        let auth_status_full = lattice_llm::auth::AuthStatus {
            providers_path: providers_path.to_path_buf(),
            providers: vec![
                provider_status_ready,
                provider_status_unconfigured,
                provider_status_declaration,
            ],
            error: Some("auth.json: permissions are too open".to_owned()),
        };
        let auth_status_minimal = lattice_llm::auth::AuthStatus {
            providers_path: providers_path.to_path_buf(),
            providers: Vec::new(),
            error: None,
        };
        assert_eq!(
            auth_status(auth_status_full),
            lattice_ipc::AuthStatus {
                providers_path: providers_path.to_path_buf(),
                providers: vec![
                    lattice_ipc::ProviderAuthStatus {
                        provider: "chatgpt".to_owned(),
                        availability: lattice_ipc::Availability::Ready,
                        email: Some("dev@example.com".to_owned()),
                        expires_at: Some("2026-06-01T00:00:00Z".to_owned()),
                        diagnostic: None,
                    },
                    lattice_ipc::ProviderAuthStatus {
                        provider: "openai".to_owned(),
                        availability: lattice_ipc::Availability::Unconfigured {
                            requirement: lattice_ipc::ConfigurationRequirement::ApiKey,
                        },
                        email: None,
                        expires_at: None,
                        diagnostic: Some(
                            "provider \"openai\" has no API key configured; run lattice auth set openai"
                                .to_owned()
                        ),
                    },
                    lattice_ipc::ProviderAuthStatus {
                        provider: "lmstudio".to_owned(),
                        availability: lattice_ipc::Availability::Unconfigured {
                            requirement: lattice_ipc::ConfigurationRequirement::Declaration,
                        },
                        email: None,
                        expires_at: None,
                        diagnostic: Some(
                            "provider \"lmstudio\" is not declared; add [providers.lmstudio] in /home/dev/.lattice/providers.toml"
                                .to_owned()
                        ),
                    },
                ],
                error: Some("auth.json: permissions are too open".to_owned()),
            },
            "auth_status/full 转换结果不符"
        );
        assert_eq!(
            auth_status(auth_status_minimal),
            lattice_ipc::AuthStatus {
                providers_path: providers_path.to_path_buf(),
                providers: Vec::new(),
                error: None,
            },
            "auth_status/minimal 转换结果不符"
        );
    }
}
