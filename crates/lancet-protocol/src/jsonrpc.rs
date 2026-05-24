//! JSON-RPC 2.0 envelope types.
//!
//! Domain-neutral request, notification, and response types conforming to
//! the JSON-RPC 2.0 specification (<https://www.jsonrpc.org/specification>).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use schemars::JsonSchema;

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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    String(String),
    Null,
}

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
            (Some(result), None) => Ok(Response::Success {
                result,
                id: raw.id,
            }),
            (None, Some(error)) => Ok(Response::Error {
                error,
                id: raw.id,
            }),
            (Some(_), Some(_)) => Err(D::Error::custom(
                "JSON-RPC response must not contain both \"result\" and \"error\"",
            )),
            (None, None) => Err(D::Error::custom(
                "JSON-RPC response must contain either \"result\" or \"error\"",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trip_with_object_params() {
        let req: Request<serde_json::Value> = Request::new(
            Id::Number(1),
            "execute".into(),
            json!({"path": "/tmp/test"}),
        );
        let serialized = serde_json::to_string(&req).unwrap();
        let deserialized: Request<serde_json::Value> =
            serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.id, Id::Number(1));
        assert_eq!(deserialized.method, "execute");
        assert_eq!(deserialized.params["path"], "/tmp/test");

        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
    }

    #[test]
    fn notification_serializes_without_id() {
        let notif: Notification<serde_json::Value> =
            Notification::new("progress".into(), json!({"percent": 50}));
        let serialized = serde_json::to_string(&notif).unwrap();
        assert!(!serialized.contains("\"id\""));

        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["method"], "progress");
    }

    #[test]
    fn success_response_round_trip() {
        let resp: Response<serde_json::Value, serde_json::Value> = Response::Success {
            result: json!({"status": "ok"}),
            id: Id::Number(42),
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: Response<serde_json::Value, serde_json::Value> =
            serde_json::from_str(&serialized).unwrap();
        assert!(matches!(deserialized, Response::Success { .. }));
    }

    #[test]
    fn error_response_round_trip() {
        let resp: Response<serde_json::Value, serde_json::Value> = Response::Error {
            error: ErrorObject {
                code: -32001,
                message: "validation failed".into(),
                data: Some(json!({"field": "path"})),
            },
            id: Id::String("abc".into()),
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: Response<serde_json::Value, serde_json::Value> =
            serde_json::from_str(&serialized).unwrap();
        assert!(matches!(deserialized, Response::Error { .. }));
    }

    #[test]
    fn response_with_both_result_and_error_rejected() {
        let json = json!({
            "jsonrpc": "2.0",
            "result": {"status": "ok"},
            "error": {"code": -32001, "message": "fail"},
            "id": 1
        });
        let result: Result<Response<serde_json::Value, serde_json::Value>, _> =
            serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn response_with_neither_result_nor_error_rejected() {
        let json = json!({
            "jsonrpc": "2.0",
            "id": 1
        });
        let result: Result<Response<serde_json::Value, serde_json::Value>, _> =
            serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn params_are_direct_method_data() {
        let req: Request<serde_json::Value> = Request::new(
            Id::Number(1),
            "read_file".into(),
            json!({"path": "/etc/hosts"}),
        );
        let serialized = serde_json::to_string(&req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        // params is the direct method data, not nested under a CEP wrapper
        assert_eq!(parsed["params"]["path"], "/etc/hosts");
        assert!(parsed["params"].get("envelope").is_none());
    }
}
