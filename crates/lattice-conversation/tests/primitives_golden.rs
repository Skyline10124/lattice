// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! ADR 0009 决策 5 迁移前的原语行为黄金测试。
//!
//! 仅通过 `lattice_conversation` 根路径导入被测类型与函数，在定义从
//! `lancet-protocol` 迁入 `lattice-conversation` 前锁定身份解析、错误表示、
//! 结构化地址序列化、`canonical_json` 与 `arguments_digest` 的逐字节行为。

use std::str::FromStr;

use lattice_conversation::{
    IdParseError, InvocationId, ModelAttemptKey, ModelMessageAddress, SessionId, ToolCallAddress,
    TurnId, arguments_digest, canonical_json,
};
use serde_json::{Value, json};

fn hex_lower(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ============================================================================
// a. SessionId、TurnId、InvocationId 合法样例
// ============================================================================

#[test]
fn session_id_valid_cases_roundtrip_and_format() {
    let cases = [
        // 取自 lancet-protocol/src/identity.rs 单测样例
        (
            "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c",
            "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c",
            "\"3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c\"",
        ),
        // 覆盖全部 16 个小写十六进制字符
        (
            "0123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdef",
            "\"0123456789abcdef0123456789abcdef\"",
        ),
        // 全零边界
        (
            "00000000000000000000000000000000",
            "00000000000000000000000000000000",
            "\"00000000000000000000000000000000\"",
        ),
        // SessionId 仅校验 32 位小写十六进制，不约束 UUID v4 版本位或 variant 位
        (
            "3f8b2c1a7d4e1f5a8b9c0d1e2f3a4b5c",
            "3f8b2c1a7d4e1f5a8b9c0d1e2f3a4b5c",
            "\"3f8b2c1a7d4e1f5a8b9c0d1e2f3a4b5c\"",
        ),
        (
            "3f8b2c1a7d4e4f5acb9c0d1e2f3a4b5c",
            "3f8b2c1a7d4e4f5acb9c0d1e2f3a4b5c",
            "\"3f8b2c1a7d4e4f5acb9c0d1e2f3a4b5c\"",
        ),
    ];

    for (input, expected_str, expected_json) in cases {
        let parsed = SessionId::parse(input).expect("合法 SessionId 应解析成功");
        assert_eq!(parsed.as_str(), expected_str, "as_str 应等于输入文本");
        assert_eq!(parsed.to_string(), expected_str, "Display 应等于输入文本");

        let serialized = serde_json::to_string(&parsed).expect("SessionId 序列化应成功");
        assert_eq!(serialized, expected_json, "JSON 编码应为带引号的文本");

        let deserialized: SessionId =
            serde_json::from_str(&serialized).expect("SessionId 反序列化应成功");
        assert_eq!(deserialized, parsed, "serde_json 往返应相等");

        let via_from_str = SessionId::from_str(input).expect("FromStr 解析应成功");
        assert_eq!(via_from_str, parsed, "parse 与 FromStr 结果应一致");
    }
}

#[test]
fn turn_id_valid_cases_cover_all_rfc4122_variants() {
    let cases = [
        // 取自 lancet-protocol/src/identity.rs 单测样例（variant 半字节 8）
        (
            "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c",
            "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c",
            "\"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c\"",
        ),
        // variant 半字节 9
        (
            "3f8b2c1a-7d4e-4f5a-9b9c-0d1e2f3a4b5c",
            "3f8b2c1a-7d4e-4f5a-9b9c-0d1e2f3a4b5c",
            "\"3f8b2c1a-7d4e-4f5a-9b9c-0d1e2f3a4b5c\"",
        ),
        // variant 半字节 a
        (
            "3f8b2c1a-7d4e-4f5a-ab9c-0d1e2f3a4b5c",
            "3f8b2c1a-7d4e-4f5a-ab9c-0d1e2f3a4b5c",
            "\"3f8b2c1a-7d4e-4f5a-ab9c-0d1e2f3a4b5c\"",
        ),
        // variant 半字节 b
        (
            "3f8b2c1a-7d4e-4f5a-bb9c-0d1e2f3a4b5c",
            "3f8b2c1a-7d4e-4f5a-bb9c-0d1e2f3a4b5c",
            "\"3f8b2c1a-7d4e-4f5a-bb9c-0d1e2f3a4b5c\"",
        ),
        // 覆盖全部 16 个小写十六进制字符
        (
            "01234567-89ab-4cde-8f01-23456789abcd",
            "01234567-89ab-4cde-8f01-23456789abcd",
            "\"01234567-89ab-4cde-8f01-23456789abcd\"",
        ),
    ];

    for (input, expected_str, expected_json) in cases {
        let parsed = TurnId::parse(input).expect("合法 TurnId 应解析成功");
        assert_eq!(parsed.as_str(), expected_str, "as_str 应等于输入文本");
        assert_eq!(parsed.to_string(), expected_str, "Display 应等于输入文本");

        let serialized = serde_json::to_string(&parsed).expect("TurnId 序列化应成功");
        assert_eq!(serialized, expected_json, "JSON 编码应为带引号的文本");

        let deserialized: TurnId =
            serde_json::from_str(&serialized).expect("TurnId 反序列化应成功");
        assert_eq!(deserialized, parsed, "serde_json 往返应相等");

        let via_from_str = TurnId::from_str(input).expect("FromStr 解析应成功");
        assert_eq!(via_from_str, parsed, "parse 与 FromStr 结果应一致");
    }
}

#[test]
fn invocation_id_valid_cases_cover_all_rfc4122_variants() {
    let cases = [
        // 取自 lancet-protocol/src/identity.rs 单测样例（variant 半字节 8）
        (
            "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c",
            "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c",
            "\"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c\"",
        ),
        // variant 半字节 9
        (
            "3f8b2c1a-7d4e-4f5a-9b9c-0d1e2f3a4b5c",
            "3f8b2c1a-7d4e-4f5a-9b9c-0d1e2f3a4b5c",
            "\"3f8b2c1a-7d4e-4f5a-9b9c-0d1e2f3a4b5c\"",
        ),
        // variant 半字节 a
        (
            "3f8b2c1a-7d4e-4f5a-ab9c-0d1e2f3a4b5c",
            "3f8b2c1a-7d4e-4f5a-ab9c-0d1e2f3a4b5c",
            "\"3f8b2c1a-7d4e-4f5a-ab9c-0d1e2f3a4b5c\"",
        ),
        // variant 半字节 b
        (
            "3f8b2c1a-7d4e-4f5a-bb9c-0d1e2f3a4b5c",
            "3f8b2c1a-7d4e-4f5a-bb9c-0d1e2f3a4b5c",
            "\"3f8b2c1a-7d4e-4f5a-bb9c-0d1e2f3a4b5c\"",
        ),
        // 覆盖全部 16 个小写十六进制字符
        (
            "01234567-89ab-4cde-8f01-23456789abcd",
            "01234567-89ab-4cde-8f01-23456789abcd",
            "\"01234567-89ab-4cde-8f01-23456789abcd\"",
        ),
    ];

    for (input, expected_str, expected_json) in cases {
        let parsed = InvocationId::parse(input).expect("合法 InvocationId 应解析成功");
        assert_eq!(parsed.as_str(), expected_str, "as_str 应等于输入文本");
        assert_eq!(parsed.to_string(), expected_str, "Display 应等于输入文本");

        let serialized = serde_json::to_string(&parsed).expect("InvocationId 序列化应成功");
        assert_eq!(serialized, expected_json, "JSON 编码应为带引号的文本");

        let deserialized: InvocationId =
            serde_json::from_str(&serialized).expect("InvocationId 反序列化应成功");
        assert_eq!(deserialized, parsed, "serde_json 往返应相等");

        let via_from_str = InvocationId::from_str(input).expect("FromStr 解析应成功");
        assert_eq!(via_from_str, parsed, "parse 与 FromStr 结果应一致");
    }
}

// ============================================================================
// b. SessionId、TurnId、InvocationId 拒绝表与 serde 反序列化拒绝
// ============================================================================

#[test]
fn session_id_rejection_table() {
    let cases = [
        // 空串
        (
            "",
            IdParseError::InvalidLength {
                expected: 32,
                actual: 0,
            },
            "invalid length: expected 32, got 0",
        ),
        // 过短（31 字节）
        (
            "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5",
            IdParseError::InvalidLength {
                expected: 32,
                actual: 31,
            },
            "invalid length: expected 32, got 31",
        ),
        // 过长（33 字节）
        (
            "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c0",
            IdParseError::InvalidLength {
                expected: 32,
                actual: 33,
            },
            "invalid length: expected 32, got 33",
        ),
        // 过长（带连字符的 36 字节 UUID 文本）
        (
            "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidLength {
                expected: 32,
                actual: 36,
            },
            "invalid length: expected 32, got 36",
        ),
        // 大写十六进制（字节 0 为数字 '3'，字节 1 为大写 'F'）
        (
            "3F8B2C1A7D4E4F5A8B9C0D1E2F3A4B5C",
            IdParseError::InvalidCharacter { position: 1 },
            "invalid character at byte 1",
        ),
        // 32 字节长度内含连字符（按非十六进制字符拒绝）
        (
            "3f8b2c1a-7d4e4f5a8b9c0d1e2f3a4b5",
            IdParseError::InvalidCharacter { position: 8 },
            "invalid character at byte 8",
        ),
        // 非十六进制字符
        (
            "3f8b2c1z7d4e4f5a8b9c0d1e2f3a4b5c",
            IdParseError::InvalidCharacter { position: 7 },
            "invalid character at byte 7",
        ),
        // 字节长度同为 32 的非 ASCII 文本（首字符 'é' 占 2 字节 UTF-8）
        (
            "é8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c",
            IdParseError::InvalidCharacter { position: 0 },
            "invalid character at byte 0",
        ),
        // 字节长度同为 32 的非 ASCII 文本（末字符 'é' 位于字节 30..32）
        (
            "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4bé",
            IdParseError::InvalidCharacter { position: 30 },
            "invalid character at byte 30",
        ),
    ];

    for (input, expected_err, expected_display) in cases {
        let err = SessionId::parse(input).expect_err("非法 SessionId 应被拒绝");
        assert_eq!(err, expected_err, "输入 {input:?} 的 IdParseError 不匹配");
        assert_eq!(
            err.to_string(),
            expected_display,
            "输入 {input:?} 的错误 Display 文本不匹配"
        );
        assert_eq!(
            SessionId::from_str(input),
            Err(expected_err),
            "FromStr 应返回相同的 IdParseError"
        );
    }

    for invalid_json in [
        "0",
        "123",
        "\"\"",
        "\"3F8B2C1A7D4E4F5A8B9C0D1E2F3A4B5C\"",
        "\"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c\"",
    ] {
        assert!(
            serde_json::from_str::<SessionId>(invalid_json).is_err(),
            "serde_json::from_str 应拒绝非法 SessionId JSON: {invalid_json}"
        );
    }
}

fn uuid_v4_rejection_cases() -> Vec<(&'static str, IdParseError, &'static str)> {
    vec![
        // 空串
        (
            "",
            IdParseError::InvalidLength {
                expected: 36,
                actual: 0,
            },
            "invalid length: expected 36, got 0",
        ),
        // 过短（35 字节）
        (
            "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5",
            IdParseError::InvalidLength {
                expected: 36,
                actual: 35,
            },
            "invalid length: expected 36, got 35",
        ),
        // 过短（32 字节无连字符）
        (
            "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c",
            IdParseError::InvalidLength {
                expected: 36,
                actual: 32,
            },
            "invalid length: expected 36, got 32",
        ),
        // 过长（37 字节）
        (
            "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5cx",
            IdParseError::InvalidLength {
                expected: 36,
                actual: 37,
            },
            "invalid length: expected 36, got 37",
        ),
        // 大写十六进制（字节 1 为 'F'）
        (
            "3F8B2C1A-7D4E-4F5A-8B9C-0D1E2F3A4B5C",
            IdParseError::InvalidCharacter { position: 1 },
            "invalid character at byte 1",
        ),
        // variant 位为大写 'A'（先经过小写十六进制检查，报 InvalidCharacter）
        (
            "3f8b2c1a-7d4e-4f5a-Ab9c-0d1e2f3a4b5c",
            IdParseError::InvalidCharacter { position: 19 },
            "invalid character at byte 19",
        ),
        // 连字符错位：分隔位 8、13、18、23 缺失连字符
        (
            "3f8b2c1a7-d4e-4f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidSeparator { position: 8 },
            "invalid separator at byte 8",
        ),
        (
            "3f8b2c1a-7d4e4-f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidSeparator { position: 13 },
            "invalid separator at byte 13",
        ),
        (
            "3f8b2c1a-7d4e-4f5a8-b9c-0d1e2f3a4b5c",
            IdParseError::InvalidSeparator { position: 18 },
            "invalid separator at byte 18",
        ),
        (
            "3f8b2c1a-7d4e-4f5a-8b9c0-d1e2f3a4b5c",
            IdParseError::InvalidSeparator { position: 23 },
            "invalid separator at byte 23",
        ),
        // 连字符错位：连字符提前到非分隔位 7（按非十六进制字符拒绝）
        (
            "3f8b2c1-a7d4e-4f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidCharacter { position: 7 },
            "invalid character at byte 7",
        ),
        // 非十六进制字符
        (
            "3f8b2c1z-7d4e-4f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidCharacter { position: 7 },
            "invalid character at byte 7",
        ),
        // 版本半字节不是 '4'
        (
            "3f8b2c1a-7d4e-1f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidVersion { found: '1' },
            "invalid version nibble '1'",
        ),
        (
            "3f8b2c1a-7d4e-0f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidVersion { found: '0' },
            "invalid version nibble '0'",
        ),
        (
            "3f8b2c1a-7d4e-5f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidVersion { found: '5' },
            "invalid version nibble '5'",
        ),
        // 非法 variant 半字节（不在 8/9/a/b 中）
        (
            "3f8b2c1a-7d4e-4f5a-cb9c-0d1e2f3a4b5c",
            IdParseError::InvalidVariant { found: 'c' },
            "invalid variant nibble 'c'",
        ),
        (
            "3f8b2c1a-7d4e-4f5a-7b9c-0d1e2f3a4b5c",
            IdParseError::InvalidVariant { found: '7' },
            "invalid variant nibble '7'",
        ),
        // 字节长度同为 36 的非 ASCII 文本（十六进制位、版本位、分隔位各一例）
        (
            "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4bé",
            IdParseError::InvalidCharacter { position: 34 },
            "invalid character at byte 34",
        ),
        (
            "3f8b2c1a-7d4e-é5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidCharacter { position: 14 },
            "invalid character at byte 14",
        ),
        (
            "3f8b2c1aé7d4-4f5a-8b9c-0d1e2f3a4b5c",
            IdParseError::InvalidSeparator { position: 8 },
            "invalid separator at byte 8",
        ),
    ]
}

#[test]
fn turn_id_rejection_table() {
    for (input, expected_err, expected_display) in uuid_v4_rejection_cases() {
        let err = TurnId::parse(input).expect_err("非法 TurnId 应被拒绝");
        assert_eq!(err, expected_err, "输入 {input:?} 的 IdParseError 不匹配");
        assert_eq!(
            err.to_string(),
            expected_display,
            "输入 {input:?} 的错误 Display 文本不匹配"
        );
        assert_eq!(
            TurnId::from_str(input),
            Err(expected_err),
            "FromStr 应返回相同的 IdParseError"
        );
    }

    for invalid_json in [
        "0",
        "123",
        "\"\"",
        "\"3F8B2C1A-7D4E-4F5A-8B9C-0D1E2F3A4B5C\"",
        "\"3f8b2c1a-7d4e-1f5a-8b9c-0d1e2f3a4b5c\"",
        "\"3f8b2c1a-7d4e-4f5a-cb9c-0d1e2f3a4b5c\"",
    ] {
        assert!(
            serde_json::from_str::<TurnId>(invalid_json).is_err(),
            "serde_json::from_str 应拒绝非法 TurnId JSON: {invalid_json}"
        );
    }
}

#[test]
fn invocation_id_rejection_table() {
    for (input, expected_err, expected_display) in uuid_v4_rejection_cases() {
        let err = InvocationId::parse(input).expect_err("非法 InvocationId 应被拒绝");
        assert_eq!(err, expected_err, "输入 {input:?} 的 IdParseError 不匹配");
        assert_eq!(
            err.to_string(),
            expected_display,
            "输入 {input:?} 的错误 Display 文本不匹配"
        );
        assert_eq!(
            InvocationId::from_str(input),
            Err(expected_err),
            "FromStr 应返回相同的 IdParseError"
        );
    }

    for invalid_json in [
        "0",
        "123",
        "\"\"",
        "\"3F8B2C1A-7D4E-4F5A-8B9C-0D1E2F3A4B5C\"",
        "\"3f8b2c1a-7d4e-1f5a-8b9c-0d1e2f3a4b5c\"",
        "\"3f8b2c1a-7d4e-4f5a-cb9c-0d1e2f3a4b5c\"",
    ] {
        assert!(
            serde_json::from_str::<InvocationId>(invalid_json).is_err(),
            "serde_json::from_str 应拒绝非法 InvocationId JSON: {invalid_json}"
        );
    }
}

// ============================================================================
// c. ModelMessageAddress、ToolCallAddress、ModelAttemptKey 序列化与往返
// ============================================================================

#[test]
fn model_message_address_serde_exact_and_roundtrip() {
    let session_id = SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").expect("固定 SessionId");
    let turn_id = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("固定 TurnId");

    let cases = [
        (
            0,
            r#"{"session_id":"3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c","turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":0}"#,
        ),
        (
            7,
            r#"{"session_id":"3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c","turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":7}"#,
        ),
        (
            u32::MAX,
            r#"{"session_id":"3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c","turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":4294967295}"#,
        ),
    ];

    for (model_step_ordinal, expected_json) in cases {
        let address = ModelMessageAddress {
            session_id,
            turn_id,
            model_step_ordinal,
        };
        let encoded = serde_json::to_string(&address).expect("ModelMessageAddress 序列化应成功");
        assert_eq!(
            encoded, expected_json,
            "ModelMessageAddress JSON 输出不匹配"
        );

        let decoded: ModelMessageAddress =
            serde_json::from_str(&encoded).expect("ModelMessageAddress 反序列化应成功");
        assert_eq!(decoded, address, "ModelMessageAddress 往返应相等");
    }
}

#[test]
fn tool_call_address_serde_exact_and_roundtrip() {
    let session_id = SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").expect("固定 SessionId");
    let turn_id = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("固定 TurnId");

    let cases = [
        (
            0,
            0,
            r#"{"message":{"session_id":"3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c","turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":0},"part_ordinal":0}"#,
        ),
        (
            2,
            5,
            r#"{"message":{"session_id":"3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c","turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":2},"part_ordinal":5}"#,
        ),
        (
            u32::MAX,
            u32::MAX,
            r#"{"message":{"session_id":"3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c","turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":4294967295},"part_ordinal":4294967295}"#,
        ),
    ];

    for (model_step_ordinal, part_ordinal, expected_json) in cases {
        let address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id,
                turn_id,
                model_step_ordinal,
            },
            part_ordinal,
        };
        let encoded = serde_json::to_string(&address).expect("ToolCallAddress 序列化应成功");
        assert_eq!(encoded, expected_json, "ToolCallAddress JSON 输出不匹配");

        let decoded: ToolCallAddress =
            serde_json::from_str(&encoded).expect("ToolCallAddress 反序列化应成功");
        assert_eq!(decoded, address, "ToolCallAddress 往返应相等");
    }
}

#[test]
fn model_attempt_key_serde_exact_and_roundtrip() {
    let turn_id = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("固定 TurnId");

    let cases = [
        (
            0,
            0,
            r#"{"turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":0,"attempt_ordinal":0}"#,
        ),
        (
            1,
            3,
            r#"{"turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":1,"attempt_ordinal":3}"#,
        ),
        (
            u32::MAX,
            u32::MAX,
            r#"{"turn_id":"3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c","model_step_ordinal":4294967295,"attempt_ordinal":4294967295}"#,
        ),
    ];

    for (model_step_ordinal, attempt_ordinal, expected_json) in cases {
        let key = ModelAttemptKey {
            turn_id,
            model_step_ordinal,
            attempt_ordinal,
        };
        let encoded = serde_json::to_string(&key).expect("ModelAttemptKey 序列化应成功");
        assert_eq!(encoded, expected_json, "ModelAttemptKey JSON 输出不匹配");

        let decoded: ModelAttemptKey =
            serde_json::from_str(&encoded).expect("ModelAttemptKey 反序列化应成功");
        assert_eq!(decoded, key, "ModelAttemptKey 往返应相等");
    }
}

// ============================================================================
// d. canonical_json 精确输出
// ============================================================================

#[test]
fn canonical_json_nested_objects_arrays_and_empty_containers() {
    let nested_a: Value = serde_json::from_str(
        r#"{"z":{"y":{"x":1,"a":2},"b":3},"a":[{"q":1,"p":2},[true,false,null],{},[]]}"#,
    )
    .expect("嵌套 JSON A");
    let nested_b: Value = serde_json::from_str(
        r#"{ "a" : [ { "p" : 2 , "q" : 1 } , [ true , false , null ] , {} , [] ] , "z" : { "b" : 3 , "y" : { "a" : 2 , "x" : 1 } } }"#,
    )
    .expect("嵌套 JSON B");

    let expected = r#"{"a":[{"p":2,"q":1},[true,false,null],{},[]],"z":{"b":3,"y":{"a":2,"x":1}}}"#;
    assert_eq!(
        canonical_json(&nested_a),
        expected,
        "乱序嵌套对象规范化不匹配"
    );
    assert_eq!(
        canonical_json(&nested_b),
        expected,
        "同义不同键序输入结果应一致"
    );

    // 数组保持原有元素顺序，内部对象的键按字节序重排
    let array_value = json!([3, 1, 2, {"b": 1, "a": 2}, [[9, 8], []]]);
    assert_eq!(
        canonical_json(&array_value),
        r#"[3,1,2,{"a":2,"b":1},[[9,8],[]]]"#,
        "数组顺序与嵌套结构输出不匹配"
    );

    assert_eq!(canonical_json(&json!({})), "{}", "空对象输出不匹配");
    assert_eq!(canonical_json(&json!([])), "[]", "空数组输出不匹配");
}

#[test]
fn canonical_json_non_ascii_and_empty_keys_follow_utf8_byte_order() {
    // '！' (U+FF01, UTF-8 EF BC 81) 在 UTF-8 字节序中排在 '🦀' (U+1F980, UTF-8 F0 9F A6 80) 之前，
    // 而 UTF-16 码元序相反；此例同时覆盖空键、大小写 ASCII 与多字节非 ASCII 键。
    let value: Value =
        serde_json::from_str(r#"{"🦀":1,"！":2,"中":3,"\u00e9":4,"a":5,"B":6,"":7}"#)
            .expect("非 ASCII 键 JSON");

    assert_eq!(
        canonical_json(&value),
        r#"{"":7,"B":6,"a":5,"é":4,"中":3,"！":2,"🦀":1}"#,
        "对象键应按 UTF-8 字节序排列并原样输出 UTF-8 字符"
    );
}

#[test]
fn canonical_json_scalars_numbers_and_string_escapes() {
    assert_eq!(canonical_json(&json!(true)), "true");
    assert_eq!(canonical_json(&json!(false)), "false");
    assert_eq!(canonical_json(&json!(null)), "null");
    assert_eq!(canonical_json(&json!(0)), "0");
    assert_eq!(canonical_json(&json!(-3)), "-3");
    assert_eq!(
        canonical_json(&json!(18_446_744_073_709_551_615_u64)),
        "18446744073709551615"
    );
    assert_eq!(canonical_json(&json!(1.5)), "1.5");
    assert_eq!(canonical_json(&json!(-1.25)), "-1.25");

    let numbers: Value = serde_json::from_str(
        r#"{"zero":0,"neg":-3,"min_i64":-9223372036854775808,"big_u64":9007199254740993,"max_u64":18446744073709551615,"pos_float":1.5,"neg_float":-1.25,"zero_float":0.0}"#,
    )
    .expect("数值 JSON");
    assert_eq!(
        canonical_json(&numbers),
        r#"{"big_u64":9007199254740993,"max_u64":18446744073709551615,"min_i64":-9223372036854775808,"neg":-3,"neg_float":-1.25,"pos_float":1.5,"zero":0,"zero_float":0.0}"#,
        "整数与浮点数规范化输出不匹配"
    );

    let escaping = json!({
        "quote": "a\"b",
        "backslash": "c\\d",
        "controls": "\u{0000}\u{0008}\t\n\u{000c}\r\u{001f}",
        "unicode": "é中文🦀",
    });
    assert_eq!(
        canonical_json(&escaping),
        r#"{"backslash":"c\\d","controls":"\u0000\b\t\n\f\r\u001f","quote":"a\"b","unicode":"é中文🦀"}"#,
        "引号、反斜杠、控制字符与非 ASCII 文本转义不匹配"
    );
}

// ============================================================================
// e. arguments_digest 黄金摘要值与键序无关性
// ============================================================================

#[test]
fn arguments_digest_golden_vectors_and_key_order_invariance() {
    // 与 crates/lattice-conversation/src/tool_call.rs:351 的既有 golden 保持一致
    let path_arg: Value = serde_json::from_str(r#"{"path":"a.txt"}"#).expect("path 参数");
    assert_eq!(
        hex_lower(&arguments_digest(&path_arg)),
        "5fd582703776c899a2ecb12f3d699bd7244dff1f443ef5ae3f0067bcf3ce279e",
        "{{\"path\":\"a.txt\"}} 的摘要必须等于既有 golden"
    );

    let empty_obj: Value = serde_json::from_str(r#"{}"#).expect("空对象参数");
    assert_eq!(
        hex_lower(&arguments_digest(&empty_obj)),
        "80164af0ef63d2fc0777ea8407179319120272dc8aa4924536175d19eadac895",
        "{{}} 的摘要不匹配"
    );

    let nested_utf8_a: Value = serde_json::from_str(
        r#"{"meta":{"作者":"林","tags":["α","β"]},"path":"目录/文件.txt","line":42}"#,
    )
    .expect("含非 ASCII 的嵌套对象 A");
    let nested_utf8_b: Value = serde_json::from_str(
        r#"{ "line" : 42 , "path" : "目录/文件.txt" , "meta" : { "tags" : [ "α" , "β" ] , "作者" : "林" } }"#,
    )
    .expect("含非 ASCII 的嵌套对象 B（换键序与空白）");

    let digest_nested_a = arguments_digest(&nested_utf8_a);
    let digest_nested_b = arguments_digest(&nested_utf8_b);
    assert_eq!(
        digest_nested_a, digest_nested_b,
        "同一对象换键序后摘要应相同"
    );
    assert_eq!(
        hex_lower(&digest_nested_a),
        "e3c56d486703bd5bf332c01c7a2f8128059affdc598eba69373229cf06694d33",
        "含非 ASCII 字符串的嵌套对象摘要不匹配"
    );

    let pair_reordered: Value = serde_json::from_str(r#"{ "b" : 2, "a" : 1 }"#).expect("乱序对象");
    let pair_canonical: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).expect("正序对象");
    let digest_reordered = arguments_digest(&pair_reordered);
    let digest_canonical = arguments_digest(&pair_canonical);
    assert_eq!(digest_reordered, digest_canonical, "换键序后摘要字节应相同");
    assert_eq!(
        hex_lower(&digest_canonical),
        "39a87151fc3c81ef075894dbb1276c1c8a40d8d826bdd2943d5b43aa94d3c8a4",
        "{{\"a\":1,\"b\":2}} 摘要不匹配"
    );
}
