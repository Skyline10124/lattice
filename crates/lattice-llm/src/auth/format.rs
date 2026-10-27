// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use serde::de::{MapAccess, Visitor, value::MapAccessDeserializer};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    version: u64,
    #[serde(deserialize_with = "providers")]
    pub(super) providers: BTreeMap<String, Credential>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            providers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Credential {
    ApiKey {
        key: Secret,
    },
    Chatgpt {
        host_id: Secret,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        client_id: Option<Secret>,
        #[serde(
            skip_serializing_if = "Option::is_none",
            default,
            deserialize_with = "optional_object"
        )]
        account: Option<Account>,
        #[serde(
            skip_serializing_if = "Option::is_none",
            default,
            deserialize_with = "optional_object"
        )]
        tokens: Option<Tokens>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Account {
    subject: Secret,
    pub(super) email: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tokens {
    access_token: Secret,
    refresh_token: Secret,
    id_token: Secret,
    #[serde(deserialize_with = "timestamp")]
    pub(super) expires_at: String,
    scopes: Vec<String>,
}

fn timestamp<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    chrono::DateTime::parse_from_rfc3339(&value)
        .map_err(|_| serde::de::Error::custom("invalid expiry"))?;
    Ok(value)
}

fn optional_object<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    object(d).map(Some)
}

fn object<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    struct Object<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Object<T> {
        type Value = T;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an object")
        }
        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }
    d.deserialize_map(Object(std::marker::PhantomData))
}

fn providers<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, Credential>, D::Error> {
    struct Providers;
    impl<'de> Visitor<'de> for Providers {
        type Value = BTreeMap<String, Credential>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("unique provider entries")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut entries = BTreeMap::new();
            struct Entry(Credential);
            impl<'de> Deserialize<'de> for Entry {
                fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                    object(d).map(Self)
                }
            }
            while let Some((id, Entry(credential))) = map.next_entry::<String, Entry>()? {
                if entries.insert(id, credential).is_some() {
                    return Err(serde::de::Error::custom("duplicate provider"));
                }
            }
            Ok(entries)
        }
    }
    d.deserialize_map(Providers)
}

pub(super) fn parse(bytes: &[u8]) -> Result<Document, AuthError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let document: Document = object(&mut deserializer)
        .map_err(|_| AuthError::InvalidFile("invalid credential structure"))?;
    deserializer
        .end()
        .map_err(|_| AuthError::InvalidFile("invalid credential structure"))?;
    if document.version != 1 {
        return Err(AuthError::InvalidFile("unsupported version (expected 1)"));
    }
    for (id, credential) in &document.providers {
        crate::provider::validate_provider_id(id)
            .map_err(|_| AuthError::InvalidFile("invalid provider id"))?;
        match credential {
            Credential::ApiKey { key } => {
                if id == "chatgpt" {
                    return Err(AuthError::InvalidFile("chatgpt requires a chatgpt entry"));
                }
                validate_key(&key.0).map_err(|_| AuthError::InvalidFile("invalid API key"))?;
            }
            Credential::Chatgpt {
                host_id,
                client_id,
                account,
                tokens,
            } => {
                if id != "chatgpt"
                    || host_id.0.is_empty()
                    || client_id.as_ref().is_some_and(|value| value.0.is_empty())
                    || account
                        .as_ref()
                        .is_some_and(|value| value.subject.0.is_empty() || value.email.is_empty())
                    || tokens.is_some() && (client_id.is_none() || account.is_none())
                {
                    return Err(AuthError::InvalidFile("invalid chatgpt entry"));
                }
                if tokens.as_ref().is_some_and(|tokens| {
                    [
                        &tokens.access_token,
                        &tokens.refresh_token,
                        &tokens.id_token,
                    ]
                    .iter()
                    .any(|value| value.0.is_empty())
                }) {
                    return Err(AuthError::InvalidFile("invalid chatgpt tokens"));
                }
            }
        }
    }
    Ok(document)
}
