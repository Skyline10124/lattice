//! v1.1 API session types.
//!
//! Phase 6 stubs: `create_session` and `open_session` return typed `ApiSession`
//! values without touching durable storage. Session persistence is deferred
//! to Phase 8.

use uuid::Uuid;

/// Errors that occur during API operations.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// A session with the requested id was not found.
    #[error("session '{0}' not found")]
    NotFound(String),

    /// An internal error occurred.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Convenience result type for API operations.
pub type Result<T> = std::result::Result<T, ApiError>;

/// Request to create a new session.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CreateSessionRequest {
    /// Optional human-readable label.
    pub label: Option<String>,
    /// Optional profile name to associate.
    pub profile: Option<String>,
    /// Optional model alias.
    pub model: Option<String>,
}

/// Request to open an existing session.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OpenSessionRequest {
    /// The session id to open.
    pub id: String,
}

/// A session handle returned by the API.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ApiSession {
    /// Unique session identifier.
    pub id: String,
    /// Human-readable label, if set at creation time.
    pub label: Option<String>,
    /// Profile associated with this session.
    pub profile: Option<String>,
    /// Model alias for this session.
    pub model: Option<String>,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
}

impl ApiSession {
    /// Create a new session handle with the given parameters.
    pub fn new(label: Option<String>, profile: Option<String>, model: Option<String>) -> Self {
        let timestamp = chrono_now();
        ApiSession {
            id: Uuid::new_v4().simple().to_string(),
            label,
            profile,
            model,
            created_at: timestamp,
        }
    }

    /// Create a stub session handle for a requested id (open).
    pub fn with_id(id: String) -> Self {
        let timestamp = chrono_now();
        ApiSession {
            id,
            label: None,
            profile: None,
            model: None,
            created_at: timestamp,
        }
    }
}

/// Create a new session.
///
/// Generates a stable id via UUID v4 and returns an [`ApiSession`] handle.
/// Does not touch disk in Phase 6.
pub fn create_session(request: CreateSessionRequest) -> Result<ApiSession> {
    Ok(ApiSession::new(request.label, request.profile, request.model))
}

/// Open an existing session by id.
///
/// Phase 6: returns `NotFound` — no durable session store exists yet.
/// Phase 8 will wire real lookup.
pub fn open_session(request: OpenSessionRequest) -> Result<ApiSession> {
    Err(ApiError::NotFound(request.id))
}

/// Return a simple RFC 3339 timestamp for Phase 6.
fn chrono_now() -> String {
    // Use a simple fixed format instead of pulling in the chrono crate.
    // The workspace has chrono available, but the API crate keeps deps narrow.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            let secs = d.as_secs();
            // Rough RFC 3339: YYYY-MM-DDTHH:MM:SSZ
            let days_since_epoch = secs / 86400;
            let time_of_day = secs % 86400;
            let hours = time_of_day / 3600;
            let minutes = (time_of_day % 3600) / 60;
            let seconds = time_of_day % 60;

            // Simplified date calculation from Unix epoch (1970-01-01).
            // Accurate enough for Phase 6 timestamps.
            let mut year = 1970i64;
            let mut remaining_days = days_since_epoch as i64;

            loop {
                let days_in_year = if is_leap(year) { 366 } else { 365 };
                if remaining_days < days_in_year {
                    break;
                }
                remaining_days -= days_in_year;
                year += 1;
            }

            let month_days = if is_leap(year) {
                [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
            } else {
                [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
            };

            let mut month = 1;
            for &md in &month_days {
                if remaining_days < md as i64 {
                    break;
                }
                remaining_days -= md as i64;
                month += 1;
            }

            let day = remaining_days + 1;
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                year, month, day, hours, minutes, seconds
            )
        })
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_session_returns_api_session_with_id() {
        let req = CreateSessionRequest {
            label: Some("test session".into()),
            profile: None,
            model: None,
        };
        let session = create_session(req).unwrap();
        assert!(!session.id.is_empty(), "session id should not be empty");
        // UUID v4 simple format is 32 hex chars.
        assert_eq!(
            session.id.len(),
            32,
            "session id should be 32 hex chars, got: {}",
            session.id
        );
        assert_eq!(session.label.as_deref(), Some("test session"));
    }

    #[test]
    fn create_session_defaults_profile_and_model_to_none() {
        let req = CreateSessionRequest {
            label: None,
            profile: None,
            model: None,
        };
        let session = create_session(req).unwrap();
        assert!(session.profile.is_none());
        assert!(session.model.is_none());
    }

    #[test]
    fn open_session_returns_not_found_in_phase_6() {
        let req = OpenSessionRequest {
            id: "abc123".to_string(),
        };
        let result = open_session(req);
        assert!(result.is_err(), "open_session should return NotFound in Phase 6");
        match result.unwrap_err() {
            ApiError::NotFound(id) => assert_eq!(id, "abc123"),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn session_created_at_is_rfc3339_ish() {
        let session = ApiSession::new(None, None, None);
        // Should contain T and Z separators.
        assert!(
            session.created_at.contains('T'),
            "created_at should contain 'T': {}",
            session.created_at
        );
        assert!(
            session.created_at.ends_with('Z'),
            "created_at should end with 'Z': {}",
            session.created_at
        );
    }
}
