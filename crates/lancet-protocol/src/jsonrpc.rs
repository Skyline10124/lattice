// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! JSON-RPC 2.0 envelope types.
//!
//! Domain-neutral request, notification, and response types conforming to
//! the JSON-RPC 2.0 specification (<https://www.jsonrpc.org/specification>).

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const JSONRPC_VERSION: &str = "2.0";

/// Private unit struct that always serializes as `"2.0"` and rejects anything else on deserialization.
#[derive(Clone, Debug, PartialEq, Eq, JsonSchema)]
struct Version;

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(JSONRPC_VERSION)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s != JSONRPC_VERSION {
            return Err(serde::de::Error::custom(format!(
                "invalid jsonrpc version: expected \"2.0\", got {:?}",
                s
            )));
        }
        Ok(Version)
    }
}

/// JSON-RPC request identifier.
///
/// Product calls use exactly the caller-allocated canonical `InvocationId`.
/// Numeric IDs, arbitrary strings, and null cannot enter the typed request or
/// response vocabulary.
pub type Id = crate::InvocationId;

/// A JSON-RPC 2.0 request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Request<P> {
    jsonrpc: Version,
    pub id: Id,
    pub method: String,
    pub params: P,
}

impl<P> Request<P> {
    pub fn new(id: Id, method: String, params: P) -> Self {
        Self {
            jsonrpc: Version,
            id,
            method,
            params,
        }
    }
}

/// A JSON-RPC 2.0 notification (no id).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Notification<P> {
    jsonrpc: Version,
    pub method: String,
    pub params: P,
}

impl<P> Notification<P> {
    pub fn new(method: String, params: P) -> Self {
        Self {
            jsonrpc: Version,
            method,
            params,
        }
    }
}

/// A JSON-RPC 2.0 error object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorObject<D> {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<D>,
}

/// A JSON-RPC 2.0 response — exactly one of `result` or `error`, never both.
#[derive(Clone, Debug, PartialEq, JsonSchema)]
pub enum Response<R, E> {
    Success { result: R, id: Id },
    Error { error: ErrorObject<E>, id: Id },
}

impl<R, E> Serialize for Response<R, E>
where
    R: Serialize,
    E: Serialize,
{
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(3))?;
        map.serialize_entry("jsonrpc", JSONRPC_VERSION)?;
        match self {
            Response::Success { result, id } => {
                map.serialize_entry("result", result)?;
                map.serialize_entry("id", id)?;
            }
            Response::Error { error, id } => {
                map.serialize_entry("error", error)?;
                map.serialize_entry("id", id)?;
            }
        }
        map.end()
    }
}

impl<'de, R, E> Deserialize<'de> for Response<R, E>
where
    R: Deserialize<'de>,
    E: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        #[derive(Deserialize)]
        struct RawResponse<R, E> {
            #[allow(dead_code)]
            jsonrpc: Version,
            result: Option<R>,
            error: Option<ErrorObject<E>>,
            id: Id,
        }

        let raw: RawResponse<R, E> = Deserialize::deserialize(d)?;

        match (raw.result, raw.error) {
            (Some(result), None) => Ok(Response::Success { result, id: raw.id }),
            (None, Some(error)) => Ok(Response::Error { error, id: raw.id }),
            (Some(_), Some(_)) => Err(D::Error::custom(
                "JSON-RPC response must not contain both \"result\" and \"error\"",
            )),
            (None, None) => Err(D::Error::custom(
                "JSON-RPC response must contain either \"result\" or \"error\"",
            )),
        }
    }
}
