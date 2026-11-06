// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 统一的 provider 与模型可用性判定及修复说明。

use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    LlmError,
    auth::{AuthError, Credentials},
    provider::{Auth, Provider},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationRequirement {
    Declaration,
    ApiKey,
    ChatgptLogin,
}

impl ConfigurationRequirement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::ApiKey => "api_key",
            Self::ChatgptLogin => "chatgpt_login",
        }
    }
}

impl fmt::Display for ConfigurationRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Availability {
    Ready,
    Unconfigured {
        requirement: ConfigurationRequirement,
    },
    Invalid {
        reason: String,
    },
}

impl Availability {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn status_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Unconfigured { .. } => "unconfigured",
            Self::Invalid { .. } => "invalid",
        }
    }

    /// 由 provider id、可用性结果与实际的 `providers.toml` 路径生成面向用户的说明。
    pub fn diagnostic(&self, provider_id: &str, providers_path: &Path) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::Unconfigured {
                requirement: ConfigurationRequirement::ApiKey,
            } => Some(format!(
                "provider {provider_id:?} has no API key configured; run lattice auth set {provider_id}"
            )),
            Self::Unconfigured {
                requirement: ConfigurationRequirement::ChatgptLogin,
            } => Some("ChatGPT is not signed in; run lattice auth login chatgpt".to_owned()),
            Self::Unconfigured {
                requirement: ConfigurationRequirement::Declaration,
            } => Some(format!(
                "provider {provider_id:?} is not declared; add [providers.{provider_id}] in {}",
                providers_path.display()
            )),
            Self::Invalid { reason } => Some(reason.clone()),
        }
    }
}

/// provider 层可用性判定：只按 `Auth` 分支，不按 provider id 分支。
pub(crate) fn provider_availability(
    provider: &Provider,
    credentials: Result<&Credentials, &AuthError>,
) -> Availability {
    match provider.auth {
        Auth::None => {
            if provider.declared {
                Availability::Ready
            } else {
                Availability::Unconfigured {
                    requirement: ConfigurationRequirement::Declaration,
                }
            }
        }
        Auth::Bearer | Auth::XApiKey | Auth::XGoogApiKey => match credentials {
            Ok(credentials) if credentials.api_key(provider.id.as_str()).is_some() => {
                Availability::Ready
            }
            Ok(_) => Availability::Unconfigured {
                requirement: ConfigurationRequirement::ApiKey,
            },
            Err(error) => Availability::Invalid {
                reason: error.to_string(),
            },
        },
        Auth::OauthChatgpt => match credentials {
            Ok(credentials) if credentials.chatgpt_logged_in() => Availability::Ready,
            Ok(_) => Availability::Unconfigured {
                requirement: ConfigurationRequirement::ChatgptLogin,
            },
            Err(error) => Availability::Invalid {
                reason: error.to_string(),
            },
        },
    }
}

/// 模型层可用性判定：provider 非 Ready 时直接返回；Ready 时再求值模型配置。
pub(crate) fn model_availability<T>(
    provider: &Provider,
    credentials: Result<&Credentials, &AuthError>,
    load_model_config: impl FnOnce() -> Result<T, LlmError>,
) -> (Availability, Option<T>) {
    let availability = provider_availability(provider, credentials);
    if availability != Availability::Ready {
        return (availability, None);
    }
    match load_model_config() {
        Ok(value) => (Availability::Ready, Some(value)),
        Err(error) => {
            let reason = match error {
                LlmError::Config { message } => message,
                other => other.to_string(),
            };
            (Availability::Invalid { reason }, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

    use serde_json::json;

    use super::*;
    use crate::{
        ProviderId,
        auth::AuthStore,
        protocol::{Features, Protocol},
    };

    fn test_provider(id: &str, auth: Auth, declared: bool) -> Provider {
        let protocol = Protocol::Responses;
        Provider {
            id: ProviderId::parse(id).unwrap(),
            protocol,
            base_url: "http://localhost:8000/v1".to_owned(),
            auth,
            declared,
            catalog: None,
            features: Features::defaults(protocol),
            models: BTreeMap::new(),
        }
    }

    #[test]
    fn provider_availability_matrix() {
        #[derive(Clone, Copy)]
        enum StoreState {
            Empty,
            WithApiKey(&'static str),
            ChatgptLoggedOut,
            ChatgptSignedIn,
            Corrupted,
        }

        for (label, id, auth, declared, store_state, expected) in [
            (
                "none undeclared",
                "custom-local",
                Auth::None,
                false,
                StoreState::Empty,
                Availability::Unconfigured {
                    requirement: ConfigurationRequirement::Declaration,
                },
            ),
            (
                "none declared",
                "custom-local",
                Auth::None,
                true,
                StoreState::Empty,
                Availability::Ready,
            ),
            (
                "none declared with corrupted auth.json",
                "custom-local",
                Auth::None,
                true,
                StoreState::Corrupted,
                Availability::Ready,
            ),
            (
                "bearer missing key",
                "custom-bearer",
                Auth::Bearer,
                true,
                StoreState::Empty,
                Availability::Unconfigured {
                    requirement: ConfigurationRequirement::ApiKey,
                },
            ),
            (
                "bearer with key",
                "custom-bearer",
                Auth::Bearer,
                true,
                StoreState::WithApiKey("custom-bearer"),
                Availability::Ready,
            ),
            (
                "bearer corrupted auth.json",
                "custom-bearer",
                Auth::Bearer,
                true,
                StoreState::Corrupted,
                Availability::Invalid {
                    reason: "auth.json: invalid credential structure".to_owned(),
                },
            ),
            (
                "x-api-key missing key",
                "custom-header",
                Auth::XApiKey,
                true,
                StoreState::Empty,
                Availability::Unconfigured {
                    requirement: ConfigurationRequirement::ApiKey,
                },
            ),
            (
                "x-api-key with key",
                "custom-header",
                Auth::XApiKey,
                true,
                StoreState::WithApiKey("custom-header"),
                Availability::Ready,
            ),
            (
                "x-api-key corrupted auth.json",
                "custom-header",
                Auth::XApiKey,
                true,
                StoreState::Corrupted,
                Availability::Invalid {
                    reason: "auth.json: invalid credential structure".to_owned(),
                },
            ),
            (
                "x-goog-api-key missing key",
                "custom-google",
                Auth::XGoogApiKey,
                true,
                StoreState::Empty,
                Availability::Unconfigured {
                    requirement: ConfigurationRequirement::ApiKey,
                },
            ),
            (
                "x-goog-api-key with key",
                "custom-google",
                Auth::XGoogApiKey,
                true,
                StoreState::WithApiKey("custom-google"),
                Availability::Ready,
            ),
            (
                "x-goog-api-key corrupted auth.json",
                "custom-google",
                Auth::XGoogApiKey,
                true,
                StoreState::Corrupted,
                Availability::Invalid {
                    reason: "auth.json: invalid credential structure".to_owned(),
                },
            ),
            (
                "chatgpt not signed in (empty)",
                "custom-chatgpt",
                Auth::OauthChatgpt,
                false,
                StoreState::Empty,
                Availability::Unconfigured {
                    requirement: ConfigurationRequirement::ChatgptLogin,
                },
            ),
            (
                "chatgpt not signed in (logged-out registration)",
                "custom-chatgpt",
                Auth::OauthChatgpt,
                false,
                StoreState::ChatgptLoggedOut,
                Availability::Unconfigured {
                    requirement: ConfigurationRequirement::ChatgptLogin,
                },
            ),
            (
                "chatgpt signed in",
                "custom-chatgpt",
                Auth::OauthChatgpt,
                false,
                StoreState::ChatgptSignedIn,
                Availability::Ready,
            ),
            (
                "chatgpt corrupted auth.json",
                "custom-chatgpt",
                Auth::OauthChatgpt,
                false,
                StoreState::Corrupted,
                Availability::Invalid {
                    reason: "auth.json: invalid credential structure".to_owned(),
                },
            ),
        ] {
            let tempdir = tempfile::tempdir().unwrap();
            let config = tempdir.path().join("config");
            fs::create_dir(&config).unwrap();
            fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
            let auth_path = config.join("auth.json");
            let write_auth = |bytes: &[u8]| {
                fs::write(&auth_path, bytes).unwrap();
                fs::set_permissions(&auth_path, fs::Permissions::from_mode(0o600)).unwrap();
            };
            match store_state {
                StoreState::Empty => {}
                StoreState::WithApiKey(provider_id) => {
                    write_auth(
                        json!({
                            "version": 1,
                            "providers": {
                                provider_id: { "type": "api_key", "key": "matrix-test-key" }
                            }
                        })
                        .to_string()
                        .as_bytes(),
                    );
                }
                StoreState::ChatgptLoggedOut => {
                    write_auth(
                        json!({
                            "version": 1,
                            "providers": {
                                "chatgpt": { "type": "chatgpt", "host_id": "host" }
                            }
                        })
                        .to_string()
                        .as_bytes(),
                    );
                }
                StoreState::ChatgptSignedIn => {
                    write_auth(
                        json!({
                            "version": 1,
                            "providers": {
                                "chatgpt": crate::auth::tests::chatgpt()
                            }
                        })
                        .to_string()
                        .as_bytes(),
                    );
                }
                StoreState::Corrupted => {
                    write_auth(b"{corrupted");
                }
            }
            let store = AuthStore::new(config.clone());
            let credentials = store.load();
            let provider = test_provider(id, auth, declared);
            let actual = provider_availability(&provider, credentials.as_ref());
            assert_eq!(actual, expected, "{label}");
        }
    }
}
