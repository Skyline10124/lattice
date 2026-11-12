// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 按领域拆分的线上形状夹具与共享固定原语。
//!
//! 所有摘要与版本值均由固定合法十六进制或 JSON 字面量构造，不调用任何摘要计算函数。

pub(super) mod conversation;
pub(super) mod frames;
pub(super) mod models;
pub(super) mod session;

use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::PathBuf};

use chrono::{DateTime, TimeZone, Timelike, Utc};
use lattice_conversation::{InvocationId, ModelId, ProviderId, SessionId, TurnId};
use lattice_session::store::{AgentName, ProjectPath, SettingsVersion, TurnTimeout};

use super::CaseBook;
use crate::{ModelRef, local::auth::AuthKey};

/// 固定的冻结配置摘要（64 位小写十六进制）。
pub(super) const CONFIG_DIGEST_HEX: &str =
    "d37f1f0dc49724083a27a078c93099241240a76143b5abbf844965978d8e1a90";

/// 固定的会话设置版本（经公开 JSON 反序列化构造）。
const SETTINGS_VERSION_JSON: &str =
    "\"8deea3a86eca9668c9a6ea4bfed03ae33bbdc1ea244400eb2f0ed45cf4a46395\"";

/// `{"path":"Cargo.toml"}` 对应的固定合法工具参数摘要。
pub(super) const ARGS_CARGO_TOML_DIGEST_HEX: &str =
    "e42335283755d9b836619aa57816517bce2cd6f1c714f088ed8df9d989e94854";

/// `{"path":"missing.txt"}` 对应的固定合法工具参数摘要。
pub(super) const ARGS_MISSING_TXT_DIGEST_HEX: &str =
    "ef856407a3f19cfe6b0b23dd05fb35417bedf67822e9db321537a8c270875f90";

/// `{"command":"sleep 10"}` 对应的固定合法工具参数摘要。
pub(super) const ARGS_SLEEP_CMD_DIGEST_HEX: &str =
    "d1589a144e1179bbdc7f364be3c82a23494eafa316bcde9b6dbbdc1cab4ab726";

/// `{"lines":42}`（12 字节）对应的固定合法工具输出内容摘要。
pub(super) const CONTENT_LINES_42_DIGEST_HEX: &str =
    "d47d0b8cf320ee8879d50c2ced1dce928fb1ef022077aae720daeec8c0649380";
pub(super) const CONTENT_LINES_42_BYTES: u64 = 12;

/// 超大工具输出材料对应的固定合法内容摘要与字节数（> 64 KiB）。
pub(super) const CONTENT_OVERSIZE_DIGEST_HEX: &str =
    "f2db82c87ef0dbca63c34720fccf7d310202539d840d7555c953bdfec7406090";
pub(super) const CONTENT_OVERSIZE_BYTES: u64 = 131_072;

pub(super) fn register_all(book: &mut CaseBook) {
    conversation::register(book);
    models::register(book);
    session::register(book);
    frames::register(book);
}

/// 解析固定的 64 位小写十六进制串为 32 字节数组，不执行任何哈希计算。
pub(super) fn hex_32(hex: &str) -> [u8; 32] {
    assert_eq!(hex.len(), 64, "固定摘要十六进制长度必须为 64");
    let mut out = [0u8; 32];
    for (idx, slot) in out.iter_mut().enumerate() {
        let slice = &hex[idx * 2..idx * 2 + 2];
        *slot = u8::from_str_radix(slice, 16)
            .unwrap_or_else(|err| panic!("固定十六进制字节 {slice:?} 解析失败: {err}"));
    }
    out
}

pub(super) fn session_id_primary() -> SessionId {
    SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").expect("固定主 SessionId 应合法")
}

pub(super) fn session_id_secondary() -> SessionId {
    SessionId::parse("9a8b7c6d5e4f4a3b8c2d1e0f9a8b7c6d").expect("固定次 SessionId 应合法")
}

pub(super) fn turn_id_1() -> TurnId {
    TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("固定 TurnId 1 应合法")
}

pub(super) fn turn_id_2() -> TurnId {
    TurnId::parse("7a6b5c4d-3e2f-4a1b-9c8d-7e6f5a4b3c2d").expect("固定 TurnId 2 应合法")
}

pub(super) fn turn_id_3() -> TurnId {
    TurnId::parse("12345678-9abc-4def-8123-456789abcdef").expect("固定 TurnId 3 应合法")
}

pub(super) fn invocation_id_1() -> InvocationId {
    InvocationId::parse("11111111-1111-4111-8111-111111111111").expect("固定 InvocationId 1 应合法")
}

pub(super) fn invocation_id_2() -> InvocationId {
    InvocationId::parse("22222222-2222-4222-8222-222222222222").expect("固定 InvocationId 2 应合法")
}

pub(super) fn agent_coder() -> AgentName {
    AgentName::parse("coder").expect("固定 AgentName 应合法")
}

pub(super) fn provider_openai() -> ProviderId {
    ProviderId::parse("openai").expect("固定 ProviderId 应合法")
}

pub(super) fn model_gpt5() -> ModelId {
    ModelId::parse("gpt-5").expect("固定 ModelId 应合法")
}

pub(super) fn model_ref_gpt5() -> ModelRef {
    ModelRef::parse("openai/gpt-5").expect("固定 ModelRef 应合法")
}

pub(super) fn turn_timeout_120s() -> TurnTimeout {
    TurnTimeout::new(120).expect("固定 TurnTimeout 应合法")
}

pub(super) fn settings_version_fixed() -> SettingsVersion {
    serde_json::from_str(SETTINGS_VERSION_JSON).expect("固定 SettingsVersion JSON 应合法")
}

/// 含非 UTF-8 字节序列的 Linux 路径，用于锁定 `path_bytes` 与 `ProjectPath` 的原始字节数组编码。
pub(super) fn non_utf8_workspace_path() -> PathBuf {
    PathBuf::from(OsStr::from_bytes(b"/workspace/proj-\xff\xfe/repo"))
}

pub(super) fn project_path_non_utf8() -> ProjectPath {
    ProjectPath::from_bytes(b"/workspace/proj-\xff\xfe/repo")
        .expect("固定非 UTF-8 ProjectPath 应合法")
}

pub(super) fn utf8_store_path() -> PathBuf {
    PathBuf::from("/var/lib/lattice/sessions.redb")
}

pub(super) fn ts_created() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 10, 20, 0)
        .single()
        .expect("固定创建时间应合法")
}

pub(super) fn ts_accepted() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 10, 20, 30)
        .single()
        .expect("固定接受时间应合法")
}

pub(super) fn ts_step_0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 10, 20, 33)
        .single()
        .expect("固定 Step 0 时间应合法")
}

pub(super) fn ts_ended() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 10, 20, 35)
        .single()
        .and_then(|dt| dt.with_nanosecond(123_456_000))
        .expect("固定结束时间应合法")
}

pub(super) fn auth_key(raw: &str) -> AuthKey {
    serde_json::from_value(serde_json::Value::String(raw.to_owned()))
        .expect("固定 AuthKey JSON 字符串应合法")
}
