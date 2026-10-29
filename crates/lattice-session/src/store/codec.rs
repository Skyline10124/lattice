// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lancet_protocol::{InvocationId, SessionId, TurnId};
use redb::{ReadableTable, TableDefinition};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

pub(super) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
pub(super) const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
pub(super) const NODES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("nodes");
pub(super) const CONFIGS: TableDefinition<&str, &[u8]> = TableDefinition::new("configs");
pub(super) const TURN_INDEX: TableDefinition<&str, &[u8]> = TableDefinition::new("turn_index");
pub(super) const INDEX: TableDefinition<&str, &[u8]> = TableDefinition::new("invocation_index");
pub(super) const CHANGES: TableDefinition<u64, &[u8]> = TableDefinition::new("invocation_changes");
pub(super) const VERSION: u32 = 5;

pub(super) fn attach_path<T>(path: &std::path::Path, result: Result<T>) -> Result<T> {
    result.map_err(|err| match err {
        StoreError::UnsupportedVersion {
            path: existing,
            table,
            key,
            found,
            supported,
        } if existing.as_os_str().is_empty() => StoreError::UnsupportedVersion {
            path: path.to_path_buf(),
            table,
            key,
            found,
            supported,
        },
        other => other,
    })
}

pub(super) fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_value(value)
        .map(|v| lancet_protocol::canonical_json(&v).into_bytes())
        .map_err(|_| invalid("record", "serialization failed"))
}
pub(super) fn encode<T: Serialize + DeserializeOwned>(value: &T) -> Result<Vec<u8>> {
    let mut out = VERSION.to_be_bytes().to_vec();
    out.extend(canonical(value)?);
    // 按最终记录（而非独立参数/结果）走同一解码路径，保留递归保护。
    // 编码失败仍是输入拒绝；persist 的未提交事务整体丢弃，不污染已存事实。
    decode::<T>("record", "candidate", &out)
        .map_err(|_| invalid("record", "encoded record exceeds decoding conditions"))?;
    Ok(out)
}
pub(super) fn payload<'a>(table: &str, key: &str, bytes: &'a [u8]) -> Result<&'a [u8]> {
    let head = bytes
        .get(..4)
        .ok_or_else(|| corrupt(table, key, "missing value version"))?;
    let version = u32::from_be_bytes(head.try_into().expect("four bytes"));
    if version != VERSION {
        return Err(StoreError::UnsupportedVersion {
            path: PathBuf::new(),
            table: table.into(),
            key: key.into(),
            found: version,
            supported: VERSION,
        });
    }
    Ok(&bytes[4..])
}

pub(super) fn decode<T: DeserializeOwned + Serialize>(
    table: &str,
    key: &str,
    bytes: &[u8],
) -> Result<T> {
    let bytes = payload(table, key, bytes)?;
    // 不回显 serde 错误：错误消息可能包含完整的未知枚举值或字段。
    let error = || corrupt(table, key, "invalid record encoding");
    let wire: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| error())?;
    let record: T = serde_json::from_slice(bytes).map_err(|_| error())?;
    let known = serde_json::to_value(&record).map_err(|_| error())?;
    if !retains_fields(&wire, &known) {
        return Err(error());
    }
    Ok(record)
}

// 复用类型的 Deserialize 不一定 deny_unknown_fields（包括内部 tag 缓冲后的结构）。
// 对照该类型实际序列化的字段树，拒绝被悄悄丢弃的字段；JSON 内容自身保留全部键。
fn retains_fields(wire: &serde_json::Value, known: &serde_json::Value) -> bool {
    use serde_json::Value;
    // 完整保留的子树（尤其是用户 JSON）无需解释字段名。
    if wire == known {
        return true;
    }
    match (wire, known) {
        (Value::Object(w), Value::Object(k)) => w.iter().all(|(name, value)| {
            let Some(expected) = k.get(name) else {
                return false;
            };
            if name == "evidence"
                && expected.get("kind").and_then(Value::as_str) == Some("recorded")
            {
                // docs/architecture/cell.md「内容与版本边界」允许版本化证据结构扩展，
                // 不包括 store envelope、三元组包装或其中引用的闭合 Cell 模型。
                return value.as_object().is_some_and(|fields| {
                    fields.iter().all(|(f, v)| match f.as_str() {
                        "kind" => true,
                        "value" => v.as_object().is_some_and(|roots| {
                            roots.iter().all(|(root, v)| {
                                expected["value"]
                                    .get(root)
                                    .is_some_and(|k| evidence_fields(v, k))
                            })
                        }),
                        _ => false,
                    })
                });
            }
            retains_fields(value, expected)
        }),
        (Value::Array(w), Value::Array(k)) => {
            w.len() == k.len() && w.iter().zip(k).all(|(w, k)| retains_fields(w, k))
        }
        _ => true,
    }
}
fn evidence_fields(wire: &serde_json::Value, known: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (wire, known) {
        (Value::Object(w), Value::Object(k)) => w.iter().all(|(name, value)| {
            k.get(name).is_none_or(|expected| {
                if name == "cell" {
                    retains_fields(value, expected)
                } else {
                    evidence_fields(value, expected)
                }
            })
        }),
        (Value::Array(w), Value::Array(k)) => {
            w.len() == k.len() && w.iter().zip(k).all(|(w, k)| evidence_fields(w, k))
        }
        _ => true,
    }
}

pub(super) fn node_key(session: SessionId, node: NodeId) -> [u8; 40] {
    let mut key = [0; 40];
    key[..32].copy_from_slice(session.to_string().as_bytes());
    key[32..].copy_from_slice(&node.0.to_be_bytes());
    key
}
pub(super) fn decode_node_key(key: &[u8]) -> Result<(SessionId, NodeId)> {
    if key.len() != 40 {
        return Err(corrupt("nodes", "key", "invalid key length"));
    }
    let id = std::str::from_utf8(&key[..32])
        .ok()
        .and_then(|s| SessionId::parse(s).ok())
        .ok_or_else(|| corrupt("nodes", "key", "invalid session key"))?;
    Ok((
        id,
        NodeId(u64::from_be_bytes(
            key[32..].try_into().expect("eight bytes"),
        )),
    ))
}
pub(super) fn config_key(digest: &[u8; 32]) -> String {
    hex_digest::encode_hex(digest)
}
pub(super) fn decode_config_key(key: &str) -> Result<[u8; 32]> {
    hex_digest::decode_hex(key).ok_or_else(|| corrupt("configs", "key", "invalid config digest"))
}
pub(super) fn table_error(e: redb::TableError) -> StoreError {
    match e {
        redb::TableError::Storage(e) => StoreError::Storage(e),
        _ => corrupt(
            "schema",
            "table",
            "missing table or incompatible table type",
        ),
    }
}

#[derive(Clone, Default, PartialEq)]
pub(super) struct Records {
    // 局部工作集只保留节点范围末键，不为分配序号装载历史正文。
    pub node_tip: Option<NodeId>,
    pub sessions: BTreeMap<SessionId, SessionRecord>,
    pub nodes: BTreeMap<(SessionId, NodeId), Node>,
    pub configs: BTreeMap<[u8; 32], StoredTurnConfig>,
    pub turn_index: BTreeMap<TurnId, NodeRef>,
    pub index: BTreeMap<InvocationId, InvocationIndexEntry>,
    pub changes: BTreeMap<ChangeSeq, StoredInvocationChange>,
    pub last_change: ChangeSeq,
}

// 两种事务的 open_table 返回类型不同；宏仅共用固定的七表读取，避免暴露通用存储接口。
macro_rules! load_records {
    ($tx:expr) => {{
        let meta = $tx.open_table(META).map_err(table_error)?;
        let read_meta = |key: &str| -> Result<Vec<u8>> {
            Ok(meta
                .get(key)?
                .ok_or_else(|| corrupt("meta", key, "missing metadata"))?
                .value()
                .to_vec())
        };
        let kind: String = decode("meta", "store_kind", &read_meta("store_kind")?)?;
        if kind != "lattice-session" {
            return Err(corrupt("meta", "store_kind", "wrong store identity"));
        }
        let version: u32 = decode(
            "meta",
            "store_format_version",
            &read_meta("store_format_version")?,
        )?;
        if version != VERSION {
            return Err(StoreError::UnsupportedVersion {
                path: PathBuf::new(),
                table: "meta".into(),
                key: "store_format_version".into(),
                found: version,
                supported: VERSION,
            });
        }
        let mut r = Records {
            last_change: decode("meta", "last_change_seq", &read_meta("last_change_seq")?)?,
            ..Default::default()
        };
        for entry in $tx.open_table(SESSIONS).map_err(table_error)?.iter()? {
            let (k, v) = entry?;
            let id = SessionId::parse(k.value())
                .map_err(|_| corrupt("sessions", "key", "invalid session id"))?;
            r.sessions
                .insert(id, decode("sessions", k.value(), v.value())?);
        }
        for entry in $tx.open_table(NODES).map_err(table_error)?.iter()? {
            let (k, v) = entry?;
            let key = decode_node_key(k.value())?;
            r.nodes.insert(
                key,
                decode("nodes", &format!("{}/{}", key.0, key.1.0), v.value())?,
            );
        }
        for entry in $tx.open_table(CONFIGS).map_err(table_error)?.iter()? {
            let (k, v) = entry?;
            let digest = decode_config_key(k.value())?;
            r.configs
                .insert(digest, decode("configs", k.value(), v.value())?);
        }
        for entry in $tx.open_table(TURN_INDEX).map_err(table_error)?.iter()? {
            let (k, v) = entry?;
            let id = TurnId::parse(k.value())
                .map_err(|_| corrupt("turn_index", "key", "invalid turn id"))?;
            r.turn_index
                .insert(id, decode("turn_index", k.value(), v.value())?);
        }
        for entry in $tx.open_table(INDEX).map_err(table_error)?.iter()? {
            let (k, v) = entry?;
            let id = InvocationId::parse(k.value())
                .map_err(|_| corrupt("invocation_index", "key", "invalid invocation id"))?;
            r.index
                .insert(id, decode("invocation_index", k.value(), v.value())?);
        }
        for entry in $tx.open_table(CHANGES).map_err(table_error)?.iter()? {
            let (k, v) = entry?;
            r.changes.insert(
                ChangeSeq(k.value()),
                decode("invocation_changes", &k.value().to_string(), v.value())?,
            );
        }
        Ok(r)
    }};
}
pub(super) fn load_read(tx: &redb::ReadTransaction) -> Result<Records> {
    load_records!(tx)
}
#[cfg(test)]
pub(super) fn load_write(tx: &redb::WriteTransaction) -> Result<Records> {
    load_records!(tx)
}

pub(super) fn initialize(tx: &redb::WriteTransaction) -> Result<()> {
    {
        let mut meta = tx.open_table(META).map_err(table_error)?;
        meta.insert(
            "store_kind",
            encode(&String::from("lattice-session"))?.as_slice(),
        )?;
        meta.insert("store_format_version", encode(&VERSION)?.as_slice())?;
        meta.insert("last_change_seq", encode(&ChangeSeq(0))?.as_slice())?;
    }
    tx.open_table(SESSIONS).map_err(table_error)?;
    tx.open_table(NODES).map_err(table_error)?;
    tx.open_table(CONFIGS).map_err(table_error)?;
    tx.open_table(TURN_INDEX).map_err(table_error)?;
    tx.open_table(INDEX).map_err(table_error)?;
    tx.open_table(CHANGES).map_err(table_error)?;
    Ok(())
}

pub(super) fn persist(
    tx: &redb::WriteTransaction,
    before: &Records,
    after: &Records,
) -> Result<()> {
    // 只发布改变的行；已关闭记录与树节点的不可变性由领域入口维护。
    {
        let mut t = tx.open_table(SESSIONS).map_err(table_error)?;
        for (k, v) in &after.sessions {
            if before.sessions.get(k) != Some(v) {
                t.insert(k.to_string().as_str(), encode(v)?.as_slice())?;
            }
        }
    }
    {
        let mut t = tx.open_table(NODES).map_err(table_error)?;
        for ((s, n), v) in &after.nodes {
            if before.nodes.get(&(*s, *n)) != Some(v) {
                t.insert(node_key(*s, *n).as_slice(), encode(v)?.as_slice())?;
            }
        }
    }
    {
        let mut t = tx.open_table(CONFIGS).map_err(table_error)?;
        for (k, v) in &after.configs {
            if before.configs.get(k) != Some(v) {
                let key = config_key(k);
                if t.get(key.as_str())?.is_none() {
                    t.insert(key.as_str(), encode(v)?.as_slice())?;
                }
            }
        }
    }
    {
        let mut t = tx.open_table(TURN_INDEX).map_err(table_error)?;
        for (k, v) in &after.turn_index {
            if before.turn_index.get(k) != Some(v) {
                t.insert(k.to_string().as_str(), encode(v)?.as_slice())?;
            }
        }
    }
    {
        let mut t = tx.open_table(INDEX).map_err(table_error)?;
        for (k, v) in &after.index {
            if before.index.get(k) != Some(v) {
                t.insert(k.to_string().as_str(), encode(v)?.as_slice())?;
            }
        }
    }
    {
        let mut t = tx.open_table(CHANGES).map_err(table_error)?;
        for (k, v) in &after.changes {
            if before.changes.get(k) != Some(v) {
                if t.get(k.0)?.is_some() {
                    return Err(corrupt(
                        "invocation_changes",
                        &k.0.to_string(),
                        "change sequence already exists",
                    ));
                }
                t.insert(k.0, encode(v)?.as_slice())?;
            }
        }
    }
    if before.last_change != after.last_change {
        tx.open_table(META)
            .map_err(table_error)?
            .insert("last_change_seq", encode(&after.last_change)?.as_slice())?;
    }
    Ok(())
}
#[cfg(test)]
pub(super) fn replace_all_for_test(tx: &redb::WriteTransaction, r: &Records) -> Result<()> {
    tx.open_table(SESSIONS)
        .map_err(table_error)?
        .retain(|_, _| false)?;
    tx.open_table(NODES)
        .map_err(table_error)?
        .retain(|_, _| false)?;
    tx.open_table(CONFIGS)
        .map_err(table_error)?
        .retain(|_, _| false)?;
    tx.open_table(TURN_INDEX)
        .map_err(table_error)?
        .retain(|_, _| false)?;
    tx.open_table(INDEX)
        .map_err(table_error)?
        .retain(|_, _| false)?;
    tx.open_table(CHANGES)
        .map_err(table_error)?
        .retain(|_, _| false)?;
    persist(tx, &Records::default(), r)
}
