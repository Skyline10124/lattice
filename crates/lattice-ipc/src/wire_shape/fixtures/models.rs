// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-ipc` 模型与凭据线上展示类型片段（模型查询、候选列表、凭据状态与吊销状态）。

use std::path::PathBuf;

use lattice_conversation::{Effort, ModelId, ProviderId};

use super::{CaseBook, model_gpt5, model_ref_gpt5, provider_openai};
use crate::{
    AuthStatus, Availability, ConfigurationRequirement, ModelCandidate, ModelCandidateQuery,
    ModelCandidates, ModelPrice, ModelWarning, ProviderAuthStatus, RevocationStatus,
};

pub(super) fn model_price_full() -> ModelPrice {
    ModelPrice {
        input: Some(1.25),
        output: Some(10.0),
        cache_read: Some(0.125),
        cache_write: Some(1.5625),
    }
}

pub(super) fn model_price_minimal() -> ModelPrice {
    ModelPrice {
        input: None,
        output: None,
        cache_read: None,
        cache_write: None,
    }
}

pub(super) fn model_candidate_full() -> ModelCandidate {
    ModelCandidate {
        provider: provider_openai(),
        model: model_gpt5(),
        name: Some("GPT-5".to_owned()),
        context: Some(400_000),
        output: Some(128_000),
        supported_efforts: vec![Effort::Low, Effort::Medium, Effort::High],
        cost: Some(model_price_full()),
        availability: Availability::Ready,
    }
}

pub(super) fn model_candidate_minimal() -> ModelCandidate {
    ModelCandidate {
        provider: ProviderId::parse("local").expect("固定 ProviderId 应合法"),
        model: ModelId::parse("plain-v1").expect("固定 ModelId 应合法"),
        name: None,
        context: None,
        output: None,
        supported_efforts: Vec::new(),
        cost: None,
        availability: Availability::Unconfigured {
            requirement: ConfigurationRequirement::ApiKey,
        },
    }
}

pub(super) fn model_warning_full() -> ModelWarning {
    ModelWarning {
        provider: Some("openai".to_owned()),
        message: "cached catalog is older than 7 days".to_owned(),
    }
}

pub(super) fn model_warning_minimal() -> ModelWarning {
    ModelWarning {
        provider: None,
        message: "using built-in model metadata".to_owned(),
    }
}

pub(super) fn model_candidates_full() -> ModelCandidates {
    ModelCandidates {
        models: vec![model_candidate_full(), model_candidate_minimal()],
        warnings: vec![model_warning_full(), model_warning_minimal()],
    }
}

pub(super) fn model_candidates_minimal() -> ModelCandidates {
    ModelCandidates {
        models: Vec::new(),
        warnings: Vec::new(),
    }
}

pub(super) fn provider_auth_status_full() -> ProviderAuthStatus {
    ProviderAuthStatus {
        provider: "chatgpt".to_owned(),
        availability: Availability::Ready,
        email: Some("dev@example.com".to_owned()),
        expires_at: Some("2026-06-01T00:00:00Z".to_owned()),
    }
}

pub(super) fn provider_auth_status_minimal() -> ProviderAuthStatus {
    ProviderAuthStatus {
        provider: "openai".to_owned(),
        availability: Availability::Unconfigured {
            requirement: ConfigurationRequirement::ApiKey,
        },
        email: None,
        expires_at: None,
    }
}

pub(super) fn auth_status_full() -> AuthStatus {
    AuthStatus {
        providers_path: PathBuf::from("/home/dev/.config/lattice/providers.toml"),
        providers: vec![provider_auth_status_full(), provider_auth_status_minimal()],
        error: Some("auth.json: permissions are too open".to_owned()),
    }
}

pub(super) fn auth_status_minimal() -> AuthStatus {
    AuthStatus {
        providers_path: PathBuf::from("/home/dev/.config/lattice/providers.toml"),
        providers: Vec::new(),
        error: None,
    }
}

pub(super) fn register(book: &mut CaseBook) {
    enum_fragments!(
        book,
        "model_candidate_query",
        ModelCandidateQuery {
            (
                "list",
                ModelCandidateQuery::List { .. } => ModelCandidateQuery::List {
                    provider: Some(provider_openai()),
                    ready_only: true,
                }
            ),
            (
                "target",
                ModelCandidateQuery::Target(_) => ModelCandidateQuery::Target(model_ref_gpt5())
            ),
        }
    );

    enum_fragments!(
        book,
        "configuration_requirement",
        ConfigurationRequirement {
            ("declaration", ConfigurationRequirement::Declaration => ConfigurationRequirement::Declaration),
            ("api_key", ConfigurationRequirement::ApiKey => ConfigurationRequirement::ApiKey),
            ("chatgpt_login", ConfigurationRequirement::ChatgptLogin => ConfigurationRequirement::ChatgptLogin),
        }
    );

    enum_fragments!(
        book,
        "availability",
        Availability {
            ("ready", Availability::Ready => Availability::Ready),
            (
                "unconfigured",
                Availability::Unconfigured { .. } => Availability::Unconfigured {
                    requirement: ConfigurationRequirement::ChatgptLogin,
                }
            ),
            (
                "invalid",
                Availability::Invalid { .. } => Availability::Invalid {
                    reason: "invalid base_url in providers.toml".to_owned(),
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "revocation_status",
        RevocationStatus {
            ("confirmed", RevocationStatus::Confirmed => RevocationStatus::Confirmed),
            ("unconfirmed", RevocationStatus::Unconfirmed => RevocationStatus::Unconfirmed),
            ("not_needed", RevocationStatus::NotNeeded => RevocationStatus::NotNeeded),
        }
    );

    book.insert_struct_pair("model_price", &model_price_full(), &model_price_minimal());
    book.insert_struct_pair(
        "model_candidate",
        &model_candidate_full(),
        &model_candidate_minimal(),
    );
    book.insert_struct_pair(
        "model_warning",
        &model_warning_full(),
        &model_warning_minimal(),
    );
    book.insert_struct_pair(
        "model_candidates",
        &model_candidates_full(),
        &model_candidates_minimal(),
    );
    book.insert_struct_pair(
        "provider_auth_status",
        &provider_auth_status_full(),
        &provider_auth_status_minimal(),
    );
    book.insert_struct_pair("auth_status", &auth_status_full(), &auth_status_minimal());
}
