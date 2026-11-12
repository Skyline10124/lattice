// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Local IPC 线上形状快照字节生成、逐字节比对、版本升级门禁及往返检查。

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::ipc::PROTOCOL_VERSION;

const UPDATE_ENV: &str = "LATTICE_UPDATE_WIRE_SHAPE";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotDocument {
    protocol_version: u8,
    cases: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct StoredVersionOnly {
    protocol_version: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateAction {
    /// 磁盘字节与期望字节完全一致，无需写入。
    Verified,
    /// 允许把新的期望字节写入快照文件。
    WriteBaseline,
}

/// 往返检查：先序列化为紧凑 JSON，再按生产解码入口解回并重新序列化，要求两次输出逐字相同。
pub(super) fn round_trip_json<T>(case_name: &str, value: &T) -> String
where
    T: Serialize + DeserializeOwned,
{
    let first = serde_json::to_string(value)
        .unwrap_or_else(|err| panic!("样例 {case_name} 首次序列化失败: {err}"));
    let decoded: T = serde_json::from_str(&first).unwrap_or_else(|err| {
        panic!("样例 {case_name} 无法按生产解码类型反序列化: {err}\n原始 JSON: {first}")
    });
    let second = serde_json::to_string(&decoded)
        .unwrap_or_else(|err| panic!("样例 {case_name} 二次序列化失败: {err}"));
    assert_eq!(
        first, second,
        "样例 {case_name} 经反序列化再序列化后线上字节发生漂移"
    );
    first
}

/// 用协议版本与按名称排序的样例表确定性生成完整快照文件字节（美化 JSON 加末尾单个换行）。
pub(super) fn render_snapshot_bytes(
    protocol_version: u8,
    cases: &BTreeMap<String, String>,
) -> Vec<u8> {
    let doc = SnapshotDocument {
        protocol_version,
        cases: cases.clone(),
    };
    let mut rendered =
        serde_json::to_string_pretty(&doc).expect("快照文档序列化为美化 JSON 不应失败");
    rendered.push('\n');
    rendered.into_bytes()
}

/// 纯判定函数：普通检查先做逐字节相等比较；仅在缺失或字节不一致时解析已存协议版本并按门禁规则裁决。
pub(super) fn evaluate_gate(
    current_version: u8,
    update_requested: bool,
    expected_bytes: &[u8],
    stored_bytes: Option<&[u8]>,
) -> Result<GateAction, String> {
    let Some(actual_bytes) = stored_bytes else {
        return if update_requested {
            Ok(GateAction::WriteBaseline)
        } else {
            Err(format!(
                "未找到 Local IPC 线上形状基线文件；若正在建立初始或升级后的基线，请显式设置 {UPDATE_ENV}=1 运行测试"
            ))
        };
    };

    if actual_bytes == expected_bytes {
        return Ok(GateAction::Verified);
    }

    let header: StoredVersionOnly = serde_json::from_slice(actual_bytes).map_err(|err| {
        format!("快照文件字节与期望不一致，且无法解析已存 protocol_version: {err}")
    })?;

    match current_version.cmp(&header.protocol_version) {
        Ordering::Equal => {
            let detail = describe_mismatch(expected_bytes, actual_bytes);
            if update_requested {
                Err(format!(
                    "拒绝在 PROTOCOL_VERSION 未递增（当前仍为 v{current_version}）的情况下通过 {UPDATE_ENV}=1 覆盖快照！\n\
                     若本次确实修改了 Local IPC 线上格式，必须同步递增 ipc::PROTOCOL_VERSION；否则请恢复线上兼容性。\n{detail}"
                ))
            } else {
                Err(format!(
                    "Local IPC 线上形状与已冻结的 v{current_version} 基线逐字节不一致！\n\
                     若有意调整线上格式，请先递增 ipc::PROTOCOL_VERSION 再以 {UPDATE_ENV}=1 更新基线。\n{detail}"
                ))
            }
        }
        Ordering::Greater => {
            if update_requested {
                Ok(GateAction::WriteBaseline)
            } else {
                Err(format!(
                    "检测到 ipc::PROTOCOL_VERSION 已从 v{} 递增至 v{current_version}，但快照基线尚未同步；\n\
                     请使用 {UPDATE_ENV}=1 重新运行本测试以写入新版本基线。",
                    header.protocol_version
                ))
            }
        }
        Ordering::Less => Err(format!(
            "当前代码 ipc::PROTOCOL_VERSION (v{current_version}) 低于快照记录的版本 (v{})，协议版本严禁回退！",
            header.protocol_version
        )),
    }
}

fn describe_mismatch(expected_bytes: &[u8], actual_bytes: &[u8]) -> String {
    let (Ok(expected), Ok(actual)) = (
        serde_json::from_slice::<SnapshotDocument>(expected_bytes),
        serde_json::from_slice::<SnapshotDocument>(actual_bytes),
    ) else {
        return "差异详情: 快照文件无法按标准 SnapshotDocument 结构解析（可能存在手工格式改动或损坏）。"
            .to_owned();
    };

    if expected.cases == actual.cases {
        return "差异详情: 协议版本与全部样例条目语义相同，但快照文件字节（缩进、空格、键顺序或换行）被手工改动。"
            .to_owned();
    }

    let expected_keys: BTreeSet<&String> = expected.cases.keys().collect();
    let actual_keys: BTreeSet<&String> = actual.cases.keys().collect();
    let mut lines = Vec::new();
    for missing in actual_keys.difference(&expected_keys) {
        lines.push(format!("  - 缺失快照项: {missing}"));
    }
    for added in expected_keys.difference(&actual_keys) {
        lines.push(format!("  + 新增快照项: {added}"));
    }
    for key in expected_keys.intersection(&actual_keys) {
        let exp_val = &expected.cases[*key];
        let act_val = &actual.cases[*key];
        if exp_val != act_val {
            lines.push(format!(
                "  ~ 线上形状变化 [{key}]:\n      基线: {act_val}\n      当前: {exp_val}"
            ));
        }
    }
    lines.join("\n")
}

fn snapshot_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("wire_shape.json")
}

pub(super) fn check_or_update_snapshot(cases: &BTreeMap<String, String>) {
    let path = snapshot_path();
    let expected_bytes = render_snapshot_bytes(PROTOCOL_VERSION, cases);
    let stored_bytes = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => panic!("读取快照文件 {} 失败: {err}", path.display()),
    };
    let update_requested = std::env::var(UPDATE_ENV).as_deref() == Ok("1");

    match evaluate_gate(
        PROTOCOL_VERSION,
        update_requested,
        &expected_bytes,
        stored_bytes.as_deref(),
    ) {
        Ok(GateAction::Verified) => {}
        Ok(GateAction::WriteBaseline) => {
            std::fs::write(&path, &expected_bytes)
                .unwrap_or_else(|err| panic!("写入快照文件 {} 失败: {err}", path.display()));
        }
        Err(message) => panic!("{message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cases(tag: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(
            "fragment/protocol/responses".to_owned(),
            format!("\"{tag}\""),
        )])
    }

    #[test]
    fn identical_bytes_pass_directly() {
        let cases = sample_cases("responses");
        let bytes = render_snapshot_bytes(63, &cases);
        assert_eq!(
            evaluate_gate(63, false, &bytes, Some(&bytes)),
            Ok(GateAction::Verified)
        );
        assert_eq!(
            evaluate_gate(63, true, &bytes, Some(&bytes)),
            Ok(GateAction::Verified)
        );
    }

    #[test]
    fn same_version_rejects_content_drift_even_when_update_requested() {
        let stored = render_snapshot_bytes(63, &sample_cases("responses"));
        let current = render_snapshot_bytes(63, &sample_cases("openai-responses"));

        let err_normal = evaluate_gate(63, false, &current, Some(&stored)).unwrap_err();
        assert!(err_normal.contains("v63"));
        assert!(err_normal.contains("fragment/protocol/responses"));

        let err_update = evaluate_gate(63, true, &current, Some(&stored)).unwrap_err();
        assert!(err_update.contains("拒绝在 PROTOCOL_VERSION 未递增"));
    }

    #[test]
    fn same_version_rejects_single_space_edit_byte_for_byte() {
        let stored_base = render_snapshot_bytes(63, &sample_cases("responses"));
        let mut tampered = String::from_utf8(stored_base.clone()).expect("UTF-8 快照文本");
        tampered = tampered.replacen(
            "  \"protocol_version\": 63,",
            "   \"protocol_version\": 63,",
            1,
        );

        let err = evaluate_gate(63, false, &stored_base, Some(tampered.as_bytes())).unwrap_err();
        assert!(err.contains("逐字节不一致"));
        assert!(err.contains("手工改动"));
    }

    #[test]
    fn higher_version_requires_explicit_update_request() {
        let stored_v63 = render_snapshot_bytes(63, &sample_cases("responses"));
        let current_v64 = render_snapshot_bytes(64, &sample_cases("openai-responses"));

        let err_without_env =
            evaluate_gate(64, false, &current_v64, Some(&stored_v63)).unwrap_err();
        assert!(err_without_env.contains("已从 v63 递增至 v64"));

        assert_eq!(
            evaluate_gate(64, true, &current_v64, Some(&stored_v63)),
            Ok(GateAction::WriteBaseline)
        );
    }

    #[test]
    fn lower_version_always_fails() {
        let stored_v63 = render_snapshot_bytes(63, &sample_cases("responses"));
        let current_v62 = render_snapshot_bytes(62, &sample_cases("responses"));

        let err_normal = evaluate_gate(62, false, &current_v62, Some(&stored_v63)).unwrap_err();
        assert!(err_normal.contains("低于快照记录的版本"));

        let err_update = evaluate_gate(62, true, &current_v62, Some(&stored_v63)).unwrap_err();
        assert!(err_update.contains("低于快照记录的版本"));
    }

    #[test]
    fn missing_snapshot_writes_only_when_update_requested() {
        let current = render_snapshot_bytes(63, &sample_cases("responses"));
        assert!(evaluate_gate(63, false, &current, None).is_err());
        assert_eq!(
            evaluate_gate(63, true, &current, None),
            Ok(GateAction::WriteBaseline)
        );
    }
}
