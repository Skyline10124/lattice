// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Strict typed identities (ADR 0005 §1).
//!
//! `InvocationId` and `TurnId` are lowercase hyphenated UUID v4 values;
//! `SessionId` is the existing production session identity (32 lowercase hex
//! characters, as produced by `Uuid::new_v4().simple()` at session creation).
//!
//! This crate validates parse, display, serde, and schema forms only. It never
//! generates either value: `lattice-api::TurnCoordinator` generates
//! `InvocationId`/`TurnId` from an injectable UUID source. There is no String
//! alias and no second newtype for the same identity.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

/// Failure to parse a strict typed identity from its canonical text form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum IdParseError {
    /// Text length differs from the canonical form.
    #[error("invalid length: expected {expected}, got {actual}")]
    InvalidLength {
        /// Expected byte length.
        expected: usize,
        /// Actual byte length.
        actual: usize,
    },
    /// A hyphen separator is missing or misplaced.
    #[error("invalid separator at byte {position}")]
    InvalidSeparator {
        /// Byte position of the violation.
        position: usize,
    },
    /// A non-lowercase-hex character appears in a hex position.
    #[error("invalid character at byte {position}")]
    InvalidCharacter {
        /// Byte position of the violation.
        position: usize,
    },
    /// The version nibble is not `4` (UUID v4).
    #[error("invalid version nibble '{found}'")]
    InvalidVersion {
        /// The nibble found at the version position.
        found: char,
    },
    /// The variant nibble is not in `8/9/a/b` (RFC 4122 variant).
    #[error("invalid variant nibble '{found}'")]
    InvalidVariant {
        /// The nibble found at the variant position.
        found: char,
    },
}

const UUID_V4_LEN: usize = 36;
const HYPHEN_POSITIONS: [usize; 4] = [8, 13, 18, 23];
const VERSION_POSITION: usize = 14;
const VARIANT_POSITION: usize = 19;
const SESSION_ID_LEN: usize = 32;

fn hex_value(byte: u8, position: usize) -> Result<u8, IdParseError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(IdParseError::InvalidCharacter { position }),
    }
}

/// Strict lowercase hyphenated UUID v4 value. Private kernel shared by the
/// public newtypes so there is exactly one parser and one Display form.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct UuidV4([u8; 16]);

impl UuidV4 {
    fn parse(raw: &str) -> Result<Self, IdParseError> {
        let bytes = raw.as_bytes();
        if bytes.len() != UUID_V4_LEN {
            return Err(IdParseError::InvalidLength {
                expected: UUID_V4_LEN,
                actual: bytes.len(),
            });
        }
        let mut value = [0u8; 16];
        let mut nibble_index = 0usize;
        for (position, &byte) in bytes.iter().enumerate() {
            if HYPHEN_POSITIONS.contains(&position) {
                if byte != b'-' {
                    return Err(IdParseError::InvalidSeparator { position });
                }
                continue;
            }
            let nibble = hex_value(byte, position)?;
            if position == VERSION_POSITION && byte != b'4' {
                return Err(IdParseError::InvalidVersion {
                    found: byte as char,
                });
            }
            if position == VARIANT_POSITION && !matches!(byte, b'8' | b'9' | b'a' | b'b') {
                return Err(IdParseError::InvalidVariant {
                    found: byte as char,
                });
            }
            if nibble_index.is_multiple_of(2) {
                value[nibble_index / 2] = nibble << 4;
            } else {
                value[nibble_index / 2] |= nibble;
            }
            nibble_index += 1;
        }
        Ok(Self(value))
    }

    fn write_canonical(&self, out: &mut String) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                out.push('-');
            }
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
}

impl fmt::Debug for UuidV4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(UUID_V4_LEN);
        self.write_canonical(&mut out);
        f.write_str(&out)
    }
}

const UUID_V4_PATTERN: &str =
    "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$";

macro_rules! strict_uuid_v4_id {
    ($(#[$meta:meta])* $name:ident, $schema_name:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(UuidV4);

        impl $name {
            /// Strictly parse the canonical lowercase hyphenated UUID v4 text
            /// form. Uppercase, unhyphenated, non-v4, and non-RFC-4122-variant
            /// spellings are rejected.
            pub fn parse(raw: &str) -> Result<Self, IdParseError> {
                Ok(Self(UuidV4::parse(raw)?))
            }

            /// The canonical lowercase hyphenated text form.
            pub fn as_str(&self) -> String {
                let mut out = String::with_capacity(UUID_V4_LEN);
                self.0.write_canonical(&mut out);
                out
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.as_str())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($schema_name, "({})"), self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                Self::parse(raw)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = <Cow<'de, str>>::deserialize(deserializer)?;
                Self::parse(&raw).map_err(D::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                $schema_name.into()
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                schemars::json_schema!({
                    "type": "string",
                    "pattern": UUID_V4_PATTERN,
                })
            }
        }
    };
}

strict_uuid_v4_id!(
    /// One Tool invocation identity (ADR 0005 §1): a strict lowercase
    /// hyphenated UUID v4. The same value identifies the conversation binding,
    /// CEP request ID, transport events/cancellation, Host execution context,
    /// terminal Cell evidence, ToolResult closure, and operator output.
    InvocationId,
    "InvocationId"
);

strict_uuid_v4_id!(
    /// One accepted user Turn identity (ADR 0005 §1): a strict lowercase
    /// hyphenated UUID v4, preserved explicitly on the Turn record and never
    /// derived late from content.
    TurnId,
    "TurnId"
);

/// Session identity: exactly 32 lowercase hexadecimal characters.
///
/// This is the existing production session identity shape
/// (`Uuid::new_v4().simple()` at session creation). The type only validates;
/// it never generates.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId([u8; 16]);

impl SessionId {
    /// Strictly parse 32 lowercase hex characters. Uppercase, hyphenated, or
    /// other-length spellings are rejected.
    pub fn parse(raw: &str) -> Result<Self, IdParseError> {
        let bytes = raw.as_bytes();
        if bytes.len() != SESSION_ID_LEN {
            return Err(IdParseError::InvalidLength {
                expected: SESSION_ID_LEN,
                actual: bytes.len(),
            });
        }
        let mut value = [0u8; 16];
        for (index, &byte) in bytes.iter().enumerate() {
            let nibble = hex_value(byte, index)?;
            if index % 2 == 0 {
                value[index / 2] = nibble << 4;
            } else {
                value[index / 2] |= nibble;
            }
        }
        Ok(Self(value))
    }

    /// The canonical 32-lowercase-hex text form.
    pub fn as_str(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(SESSION_ID_LEN);
        for byte in self.0.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId({})", self.as_str())
    }
}

impl FromStr for SessionId {
    type Err = IdParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl Serialize for SessionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&raw).map_err(D::Error::custom)
    }
}

impl JsonSchema for SessionId {
    fn schema_name() -> Cow<'static, str> {
        "SessionId".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{32}$",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_V4: &str = "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const VALID_SESSION: &str = "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c";

    #[test]
    fn invocation_id_parse_display_roundtrip() {
        let id = InvocationId::parse(VALID_V4).expect("valid v4");
        assert_eq!(id.as_str(), VALID_V4);
        assert_eq!(id.to_string(), VALID_V4);
        let reparsed: InvocationId = VALID_V4.parse().expect("from_str");
        assert_eq!(id, reparsed);
    }

    #[test]
    fn turn_id_parse_display_roundtrip() {
        let id = TurnId::parse(VALID_V4).expect("valid v4");
        assert_eq!(id.to_string(), VALID_V4);
    }

    #[test]
    fn invocation_and_turn_are_distinct_types_with_shared_rules() {
        // Same text parses into both newtypes; the types remain distinct so a
        // Turn cannot be used where an Invocation is required.
        let invocation = InvocationId::parse(VALID_V4).expect("valid");
        let turn = TurnId::parse(VALID_V4).expect("valid");
        assert_eq!(invocation.as_str(), turn.as_str());
        let negative = "3f8b2c1a-7d4e-1f5a-8b9c-0d1e2f3a4b5c"; // v1, not v4
        assert!(matches!(
            InvocationId::parse(negative),
            Err(IdParseError::InvalidVersion { .. })
        ));
        assert!(matches!(
            TurnId::parse(negative),
            Err(IdParseError::InvalidVersion { .. })
        ));
    }

    #[test]
    fn rejects_uppercase() {
        let upper = VALID_V4.to_uppercase();
        assert!(matches!(
            InvocationId::parse(&upper),
            Err(IdParseError::InvalidCharacter { .. })
        ));
    }

    #[test]
    fn rejects_unhyphenated() {
        let simple = VALID_V4.replace('-', "");
        assert!(matches!(
            InvocationId::parse(&simple),
            Err(IdParseError::InvalidLength { .. })
        ));
    }

    #[test]
    fn rejects_misplaced_separator() {
        let bad = "3f8b2c1a7-d4e-4f5a-8b9c-0d1e2f3a4b5c";
        assert!(matches!(
            InvocationId::parse(bad),
            Err(IdParseError::InvalidSeparator { position: 8 })
        ));
    }

    #[test]
    fn rejects_non_hex_character() {
        let bad = "3f8b2c1z-7d4e-4f5a-8b9c-0d1e2f3a4b5c";
        assert!(matches!(
            InvocationId::parse(bad),
            Err(IdParseError::InvalidCharacter { position: 7 })
        ));
    }

    #[test]
    fn rejects_non_rfc4122_variant() {
        // Variant nibble 'c' is not one of 8/9/a/b.
        let bad = "3f8b2c1a-7d4e-4f5a-cb9c-0d1e2f3a4b5c";
        assert!(matches!(
            InvocationId::parse(bad),
            Err(IdParseError::InvalidVariant { .. })
        ));
    }

    #[test]
    fn rejects_wrong_lengths() {
        assert!(matches!(
            InvocationId::parse(""),
            Err(IdParseError::InvalidLength { .. })
        ));
        assert!(matches!(
            InvocationId::parse(&format!("{VALID_V4}x")),
            Err(IdParseError::InvalidLength { .. })
        ));
        assert!(matches!(
            InvocationId::parse(&VALID_V4[..35]),
            Err(IdParseError::InvalidLength { .. })
        ));
    }

    #[test]
    fn serde_roundtrip_and_strict_deserialize() {
        let id = InvocationId::parse(VALID_V4).expect("valid");
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, format!("\"{VALID_V4}\""));
        let back: InvocationId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);

        let upper = format!("\"{}\"", VALID_V4.to_uppercase());
        assert!(serde_json::from_str::<InvocationId>(&upper).is_err());
        assert!(serde_json::from_str::<InvocationId>("123").is_err());
    }

    #[test]
    fn json_schema_is_pattern_constrained_string() {
        let mut generator = SchemaGenerator::default();
        let schema = InvocationId::json_schema(&mut generator);
        let value = serde_json::to_value(&schema).expect("schema to value");
        assert_eq!(value["type"], "string");
        assert_eq!(value["pattern"].as_str().expect("pattern"), UUID_V4_PATTERN);
    }

    #[test]
    fn session_id_parse_display_roundtrip() {
        let id = SessionId::parse(VALID_SESSION).expect("valid session id");
        assert_eq!(id.as_str(), VALID_SESSION);
        assert_eq!(id.to_string(), VALID_SESSION);
    }

    #[test]
    fn session_id_rejects_noncanonical_spellings() {
        // Hyphenated UUID spelling is not the session identity form.
        assert!(matches!(
            SessionId::parse(VALID_V4),
            Err(IdParseError::InvalidLength { .. })
        ));
        // Uppercase hex is rejected.
        assert!(matches!(
            SessionId::parse(&VALID_SESSION.to_uppercase()),
            Err(IdParseError::InvalidCharacter { .. })
        ));
        // Wrong length.
        assert!(matches!(
            SessionId::parse("abcd"),
            Err(IdParseError::InvalidLength { .. })
        ));
    }

    #[test]
    fn session_id_serde_and_schema() {
        let id = SessionId::parse(VALID_SESSION).expect("valid");
        let json = serde_json::to_string(&id).expect("serialize");
        let back: SessionId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
        assert!(serde_json::from_str::<SessionId>("\"ABCD\"").is_err());

        let mut generator = SchemaGenerator::default();
        let schema = SessionId::json_schema(&mut generator);
        let value = serde_json::to_value(&schema).expect("schema to value");
        assert_eq!(
            value["pattern"].as_str().expect("pattern"),
            "^[0-9a-f]{32}$"
        );
    }
}
