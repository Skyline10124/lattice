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
    lattice_ipc::AuthStatus {
        providers_path,
        providers: providers.into_iter().map(provider_auth_status).collect(),
        error,
    }
}

pub(super) fn provider_auth_status(
    status: lattice_llm::auth::ProviderAuthStatus,
) -> lattice_ipc::ProviderAuthStatus {
    let lattice_llm::auth::ProviderAuthStatus {
        provider,
        availability: status_availability,
        email,
        expires_at,
    } = status;
    lattice_ipc::ProviderAuthStatus {
        provider,
        availability: availability(status_availability),
        email,
        expires_at,
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
fn llm_availability(value: &lattice_ipc::Availability) -> lattice_llm::Availability {
    match value {
        lattice_ipc::Availability::Ready => lattice_llm::Availability::Ready,
        lattice_ipc::Availability::Unconfigured { requirement } => {
            lattice_llm::Availability::Unconfigured {
                requirement: llm_configuration_requirement(*requirement),
            }
        }
        lattice_ipc::Availability::Invalid { reason } => lattice_llm::Availability::Invalid {
            reason: reason.clone(),
        },
    }
}

#[cfg(test)]
fn llm_configuration_requirement(
    value: lattice_ipc::ConfigurationRequirement,
) -> lattice_llm::ConfigurationRequirement {
    match value {
        lattice_ipc::ConfigurationRequirement::Declaration => {
            lattice_llm::ConfigurationRequirement::Declaration
        }
        lattice_ipc::ConfigurationRequirement::ApiKey => {
            lattice_llm::ConfigurationRequirement::ApiKey
        }
        lattice_ipc::ConfigurationRequirement::ChatgptLogin => {
            lattice_llm::ConfigurationRequirement::ChatgptLogin
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use lattice_conversation::{Effort, ModelId, ModelRef, ProviderId};
    use serde::Serialize;

    use super::*;

    fn assert_wire_bytes_identical<L: Serialize, I: Serialize>(label: &str, llm: &L, ipc: &I) {
        let llm_bytes = serde_json::to_vec(llm).expect("llm 类型序列化应成功");
        let ipc_bytes = serde_json::to_vec(ipc).expect("ipc 展示类型序列化应成功");
        assert_eq!(
            ipc_bytes,
            llm_bytes,
            "{label} 转换前后的 JSON 字节必须逐字一致:\n  llm: {}\n  ipc: {}",
            String::from_utf8_lossy(&llm_bytes),
            String::from_utf8_lossy(&ipc_bytes),
        );
    }

    #[test]
    fn enum_conversions_cover_every_branch_with_identical_wire_bytes() {
        for (label, ipc_q) in [
            (
                "query/list_some",
                lattice_ipc::ModelCandidateQuery::List {
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
            ),
            (
                "query/target",
                lattice_ipc::ModelCandidateQuery::Target(ModelRef::parse("openai/gpt-5").unwrap()),
            ),
        ] {
            let llm_q = model_query(&ipc_q);
            assert_wire_bytes_identical(label, &llm_q, &ipc_q);
        }

        for (label, llm_req) in [
            (
                "requirement/declaration",
                lattice_llm::ConfigurationRequirement::Declaration,
            ),
            (
                "requirement/api_key",
                lattice_llm::ConfigurationRequirement::ApiKey,
            ),
            (
                "requirement/chatgpt_login",
                lattice_llm::ConfigurationRequirement::ChatgptLogin,
            ),
        ] {
            let ipc_req = configuration_requirement(llm_req);
            assert_wire_bytes_identical(label, &llm_req, &ipc_req);
            assert_eq!(llm_configuration_requirement(ipc_req), llm_req);
        }

        for (label, llm_avail) in [
            ("availability/ready", lattice_llm::Availability::Ready),
            (
                "availability/unconfigured_declaration",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::Declaration,
                },
            ),
            (
                "availability/unconfigured_api_key",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::ApiKey,
                },
            ),
            (
                "availability/unconfigured_chatgpt_login",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::ChatgptLogin,
                },
            ),
            (
                "availability/invalid",
                lattice_llm::Availability::Invalid {
                    reason: "invalid base_url in providers.toml".to_owned(),
                },
            ),
        ] {
            let ipc_avail = availability(llm_avail.clone());
            assert_wire_bytes_identical(label, &llm_avail, &ipc_avail);
            assert_eq!(llm_availability(&ipc_avail), llm_avail);
        }

        for (label, llm_rev) in [
            (
                "revocation/confirmed",
                lattice_llm::auth::RevocationStatus::Confirmed,
            ),
            (
                "revocation/unconfirmed",
                lattice_llm::auth::RevocationStatus::Unconfirmed,
            ),
            (
                "revocation/not_needed",
                lattice_llm::auth::RevocationStatus::NotNeeded,
            ),
        ] {
            let ipc_rev = revocation_status(llm_rev);
            assert_wire_bytes_identical(label, &llm_rev, &ipc_rev);
        }
    }

    #[test]
    fn struct_conversions_cover_full_and_minimal_with_identical_wire_bytes() {
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
        assert_wire_bytes_identical(
            "model_price/full",
            &price_full,
            &model_price(price_full.clone()),
        );
        assert_wire_bytes_identical(
            "model_price/minimal",
            &price_minimal,
            &model_price(price_minimal.clone()),
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
        assert_wire_bytes_identical(
            "model_candidate/full",
            &candidate_full,
            &model_candidate(candidate_full.clone()),
        );
        assert_wire_bytes_identical(
            "model_candidate/minimal",
            &candidate_minimal,
            &model_candidate(candidate_minimal.clone()),
        );

        let warning_full = lattice_llm::ModelWarning {
            provider: Some("openai".to_owned()),
            message: "cached catalog is older than 7 days".to_owned(),
        };
        let warning_minimal = lattice_llm::ModelWarning {
            provider: None,
            message: "using built-in model metadata".to_owned(),
        };
        assert_wire_bytes_identical(
            "model_warning/full",
            &warning_full,
            &model_warning(warning_full.clone()),
        );
        assert_wire_bytes_identical(
            "model_warning/minimal",
            &warning_minimal,
            &model_warning(warning_minimal.clone()),
        );

        let candidates_full = lattice_llm::ModelCandidates {
            models: vec![candidate_full, candidate_minimal],
            warnings: vec![warning_full, warning_minimal],
        };
        let candidates_minimal = lattice_llm::ModelCandidates {
            models: Vec::new(),
            warnings: Vec::new(),
        };
        assert_wire_bytes_identical(
            "model_candidates/full",
            &candidates_full,
            &model_candidates(candidates_full.clone()),
        );
        assert_wire_bytes_identical(
            "model_candidates/minimal",
            &candidates_minimal,
            &model_candidates(candidates_minimal.clone()),
        );

        let provider_status_full = lattice_llm::auth::ProviderAuthStatus {
            provider: "chatgpt".to_owned(),
            availability: lattice_llm::Availability::Ready,
            email: Some("dev@example.com".to_owned()),
            expires_at: Some("2026-06-01T00:00:00Z".to_owned()),
        };
        let provider_status_minimal = lattice_llm::auth::ProviderAuthStatus {
            provider: "openai".to_owned(),
            availability: lattice_llm::Availability::Unconfigured {
                requirement: lattice_llm::ConfigurationRequirement::ApiKey,
            },
            email: None,
            expires_at: None,
        };
        assert_wire_bytes_identical(
            "provider_auth_status/full",
            &provider_status_full,
            &provider_auth_status(provider_status_full.clone()),
        );
        assert_wire_bytes_identical(
            "provider_auth_status/minimal",
            &provider_status_minimal,
            &provider_auth_status(provider_status_minimal.clone()),
        );

        let auth_status_full = lattice_llm::auth::AuthStatus {
            providers_path: PathBuf::from("/home/dev/.config/lattice/providers.toml"),
            providers: vec![provider_status_full, provider_status_minimal],
            error: Some("auth.json: permissions are too open".to_owned()),
        };
        let auth_status_minimal = lattice_llm::auth::AuthStatus {
            providers_path: PathBuf::from("/home/dev/.config/lattice/providers.toml"),
            providers: Vec::new(),
            error: None,
        };
        assert_wire_bytes_identical(
            "auth_status/full",
            &auth_status_full,
            &auth_status(auth_status_full.clone()),
        );
        assert_wire_bytes_identical(
            "auth_status/minimal",
            &auth_status_minimal,
            &auth_status(auth_status_minimal.clone()),
        );
    }

    #[test]
    fn availability_diagnostic_matches_llm_diagnostic_for_every_variant() {
        let providers_path = Path::new("/tmp/custom-home/.lattice/providers.toml");
        for (provider, llm_avail, ipc_avail) in [
            (
                "openai",
                lattice_llm::Availability::Ready,
                lattice_ipc::Availability::Ready,
            ),
            (
                "lmstudio",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::Declaration,
                },
                lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::Declaration,
                },
            ),
            (
                "anthropic",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::ApiKey,
                },
                lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::ApiKey,
                },
            ),
            (
                "chatgpt",
                lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::ChatgptLogin,
                },
                lattice_ipc::Availability::Unconfigured {
                    requirement: lattice_ipc::ConfigurationRequirement::ChatgptLogin,
                },
            ),
            (
                "custom",
                lattice_llm::Availability::Invalid {
                    reason: "auth.json: invalid credential structure".to_owned(),
                },
                lattice_ipc::Availability::Invalid {
                    reason: "auth.json: invalid credential structure".to_owned(),
                },
            ),
        ] {
            match &llm_avail {
                lattice_llm::Availability::Ready => {}
                lattice_llm::Availability::Unconfigured { requirement } => match requirement {
                    lattice_llm::ConfigurationRequirement::Declaration
                    | lattice_llm::ConfigurationRequirement::ApiKey
                    | lattice_llm::ConfigurationRequirement::ChatgptLogin => {}
                },
                lattice_llm::Availability::Invalid { reason: _ } => {}
            }
            match &ipc_avail {
                lattice_ipc::Availability::Ready => {}
                lattice_ipc::Availability::Unconfigured { requirement } => match requirement {
                    lattice_ipc::ConfigurationRequirement::Declaration
                    | lattice_ipc::ConfigurationRequirement::ApiKey
                    | lattice_ipc::ConfigurationRequirement::ChatgptLogin => {}
                },
                lattice_ipc::Availability::Invalid { reason: _ } => {}
            }
            assert_eq!(availability(llm_avail.clone()), ipc_avail);
            assert_eq!(
                ipc_avail.diagnostic(provider, providers_path),
                llm_avail.diagnostic(provider, providers_path),
                "provider={provider} avail={llm_avail:?}"
            );
        }
    }
}
