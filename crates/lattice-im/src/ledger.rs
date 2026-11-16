// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! IM 运行态台账（`<state-root>/lattice/im/ledger`）：维护对话会话绑定、入站事件相位流转与滑动窗口清理。

use std::{collections::BTreeMap, ffi::OsStr, fmt, sync::Arc};

use chrono::{DateTime, Utc};
use lattice_ipc::{SessionId, TurnId};
use lattice_store::{PrivateDirectory, PrivateFileError, PrivateFileOperation, StateLayout};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor},
};
use tokio::sync::Mutex;

use crate::identity::{ChatKey, InboundId};

const IM_STATE_SUBDIR: &str = "im";
const LEDGER_FILE_NAME: &str = "ledger";
const LEDGER_VERSION: u32 = 1;
const MAX_LEDGER_BYTES: usize = 16 * 1024 * 1024;

/// 台账读写与校验错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LedgerError {
    /// 台账目录或文件权限、属主、类型或大小违规。
    #[error("im/ledger：{0}")]
    InvalidFile(&'static str),
    /// 台账内容损坏、版本不符或相位组合非法（严禁覆盖为空台账）。
    #[error("im/ledger：台账内容损坏或格式不受支持（{0}）")]
    Corrupt(&'static str),
    /// 系统 I/O 或阻塞任务执行失败。
    #[error("im/ledger：{0}")]
    Io(String),
    /// 所属网关实例已退出或被宿主强制中止，阻止滞后的阻塞写入覆盖新实例台账。
    #[error("im/ledger：所属网关实例已退出，已取消滞后的台账发布")]
    PublicationClosed,
}

impl From<PrivateFileError> for LedgerError {
    fn from(err: PrivateFileError) -> Self {
        map_private_error(err)
    }
}

/// 单代网关实例的台账发布门闩：在原子重命名临界区内与实例生命周期同步，
/// 防止强制中止实例后滞留于阻塞线程池中的写入任务覆盖新实例数据。
#[derive(Clone, Debug)]
pub(crate) struct LedgerPublicationLatch {
    open: Arc<std::sync::Mutex<bool>>,
}

impl Default for LedgerPublicationLatch {
    fn default() -> Self {
        Self::new()
    }
}

impl LedgerPublicationLatch {
    pub(crate) fn new() -> Self {
        Self {
            open: Arc::new(std::sync::Mutex::new(true)),
        }
    }

    pub(crate) fn close(&self) {
        let mut guard = self.open.lock().expect("台账发布门闩锁不应中毒");
        *guard = false;
    }
}

/// 绑定单次 `PreparedImGateway::run` 生命周期的 RAII 门闩守卫：无论正常返回、异常展开还是强制取消，丢弃时立即封闭发布。
pub(crate) struct LedgerPublicationGuard {
    latch: LedgerPublicationLatch,
}

impl LedgerPublicationGuard {
    pub(crate) fn new(latch: LedgerPublicationLatch) -> Self {
        Self { latch }
    }
}

impl Drop for LedgerPublicationGuard {
    fn drop(&mut self) {
        self.latch.close();
    }
}

fn replace_ledger_guarded(
    im_dir: &PrivateDirectory,
    bytes: &[u8],
    latch: &LedgerPublicationLatch,
) -> Result<(), LedgerError> {
    im_dir.replace_guarded(
        OsStr::new(LEDGER_FILE_NAME),
        bytes,
        || {
            let guard = latch.open.lock().expect("台账发布门闩锁不应中毒");
            if !*guard {
                return Err(LedgerError::PublicationClosed);
            }
            Ok(guard)
        },
        |_guard| {},
    )
}

/// 单条入站事件在台账中的持久相位记录。
///
/// 台账只记录身份键、相位与投递完成时间，不保存消息正文、回复正文、平台凭据、轮询游标或草稿编号。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InboundRecord {
    /// 已登记并确认平台，尚未得出后台准入结果。
    Registered {
        chat: ChatKey,
        session: SessionId,
        turn: TurnId,
    },
    /// 后台已持久接受该轮次，等待终态并发送最终回复。
    Accepted {
        chat: ChatKey,
        session: SessionId,
        turn: TurnId,
    },
    /// 普通消息已发出最终回复（或遭平台 400/403 拒收）。
    DeliveredTurn {
        chat: ChatKey,
        session: SessionId,
        turn: TurnId,
        delivered_at: DateTime<Utc>,
    },
    /// `/new` 命令已在同一次写入中清除会话绑定并完成登记。
    DeliveredReset {
        chat: ChatKey,
        delivered_at: DateTime<Utc>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LedgerData {
    bindings: BTreeMap<ChatKey, SessionId>,
    inbound: BTreeMap<InboundId, InboundRecord>,
}

#[derive(Serialize)]
struct LedgerWireWrite<'a> {
    version: u32,
    bindings: &'a BTreeMap<ChatKey, SessionId>,
    inbound: BTreeMap<&'a InboundId, RawInboundEntryWrite<'a>>,
}

#[derive(Serialize)]
struct RawInboundEntryWrite<'a> {
    phase: &'static str,
    chat: &'a ChatKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn: Option<TurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivered_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerWireRead {
    version: u32,
    #[serde(deserialize_with = "deserialize_unique_btreemap")]
    bindings: BTreeMap<ChatKey, SessionId>,
    #[serde(deserialize_with = "deserialize_unique_btreemap")]
    inbound: BTreeMap<InboundId, RawInboundEntryRead>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInboundEntryRead {
    phase: String,
    chat: ChatKey,
    #[serde(default)]
    session: Option<SessionId>,
    #[serde(default)]
    turn: Option<TurnId>,
    #[serde(default)]
    delivered_at: Option<DateTime<Utc>>,
}

fn deserialize_unique_btreemap<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    struct UniqueMapVisitor<K, V>(std::marker::PhantomData<fn() -> (K, V)>);

    impl<'de, K, V> Visitor<'de> for UniqueMapVisitor<K, V>
    where
        K: Ord + Deserialize<'de>,
        V: Deserialize<'de>,
    {
        type Value = BTreeMap<K, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("不含重复键的映射表")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<K, V>()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("台账映射表包含重复键"));
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(UniqueMapVisitor(std::marker::PhantomData))
}

impl RawInboundEntryRead {
    fn into_record(self) -> Result<InboundRecord, LedgerError> {
        match self.phase.as_str() {
            "registered" => match (self.session, self.turn, self.delivered_at) {
                (Some(session), Some(turn), None) => Ok(InboundRecord::Registered {
                    chat: self.chat,
                    session,
                    turn,
                }),
                _ => Err(LedgerError::Corrupt(
                    "registered 条目必须同时包含 session 与 turn，且不得包含 delivered_at",
                )),
            },
            "accepted" => match (self.session, self.turn, self.delivered_at) {
                (Some(session), Some(turn), None) => Ok(InboundRecord::Accepted {
                    chat: self.chat,
                    session,
                    turn,
                }),
                _ => Err(LedgerError::Corrupt(
                    "accepted 条目必须同时包含 session 与 turn，且不得包含 delivered_at",
                )),
            },
            "delivered" => {
                let Some(delivered_at) = self.delivered_at else {
                    return Err(LedgerError::Corrupt("delivered 条目缺少 delivered_at"));
                };
                match (self.session, self.turn) {
                    (Some(session), Some(turn)) => Ok(InboundRecord::DeliveredTurn {
                        chat: self.chat,
                        session,
                        turn,
                        delivered_at,
                    }),
                    (None, None) => Ok(InboundRecord::DeliveredReset {
                        chat: self.chat,
                        delivered_at,
                    }),
                    _ => Err(LedgerError::Corrupt(
                        "delivered 条目的 session 与 turn 必须同时存在或同时省略",
                    )),
                }
            }
            _ => Err(LedgerError::Corrupt("未知入站相位标签")),
        }
    }
}

fn parse_ledger_bytes(bytes: &[u8]) -> Result<LedgerData, LedgerError> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let wire = LedgerWireRead::deserialize(&mut de)
        .map_err(|_| LedgerError::Corrupt("JSON 语法、字段或键约束校验失败"))?;
    de.end()
        .map_err(|_| LedgerError::Corrupt("JSON 末尾包含多余尾随字节"))?;
    if wire.version != LEDGER_VERSION {
        return Err(LedgerError::Corrupt("版本号不受支持"));
    }
    let mut inbound = BTreeMap::new();
    for (id, raw) in wire.inbound {
        inbound.insert(id, raw.into_record()?);
    }
    Ok(LedgerData {
        bindings: wire.bindings,
        inbound,
    })
}

fn serialize_ledger_data(data: &LedgerData) -> Result<Vec<u8>, LedgerError> {
    let mut inbound = BTreeMap::new();
    for (id, record) in &data.inbound {
        let raw = match record {
            InboundRecord::Registered {
                chat,
                session,
                turn,
            } => RawInboundEntryWrite {
                phase: "registered",
                chat,
                session: Some(*session),
                turn: Some(*turn),
                delivered_at: None,
            },
            InboundRecord::Accepted {
                chat,
                session,
                turn,
            } => RawInboundEntryWrite {
                phase: "accepted",
                chat,
                session: Some(*session),
                turn: Some(*turn),
                delivered_at: None,
            },
            InboundRecord::DeliveredTurn {
                chat,
                session,
                turn,
                delivered_at,
            } => RawInboundEntryWrite {
                phase: "delivered",
                chat,
                session: Some(*session),
                turn: Some(*turn),
                delivered_at: Some(*delivered_at),
            },
            InboundRecord::DeliveredReset { chat, delivered_at } => RawInboundEntryWrite {
                phase: "delivered",
                chat,
                session: None,
                turn: None,
                delivered_at: Some(*delivered_at),
            },
        };
        inbound.insert(id, raw);
    }
    let wire = LedgerWireWrite {
        version: LEDGER_VERSION,
        bindings: &data.bindings,
        inbound,
    };
    let mut bytes = serde_json::to_vec_pretty(&wire)
        .map_err(|_| LedgerError::Io("序列化台账 JSON 失败".into()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn serialize_checked_ledger_data(
    data: &LedgerData,
    max_bytes: usize,
) -> Result<Vec<u8>, LedgerError> {
    let bytes = serialize_ledger_data(data)?;
    if bytes.len() > max_bytes {
        return Err(LedgerError::InvalidFile("台账文件超过 16 MiB 大小上限"));
    }
    Ok(bytes)
}

fn prune_expired_delivered(
    data: &mut LedgerData,
    now: DateTime<Utc>,
    redelivery_window: chrono::Duration,
) -> bool {
    let before_len = data.inbound.len();
    data.inbound.retain(|_, record| match record {
        InboundRecord::DeliveredTurn { delivered_at, .. }
        | InboundRecord::DeliveredReset { delivered_at, .. } => {
            now.signed_duration_since(*delivered_at) <= redelivery_window
        }
        InboundRecord::Registered { .. } | InboundRecord::Accepted { .. } => true,
    });
    data.inbound.len() != before_len
}

fn map_private_error(error: PrivateFileError) -> LedgerError {
    match error {
        PrivateFileError::InvalidName => {
            LedgerError::InvalidFile("状态目录或台账文件名包含非法路径分量")
        }
        PrivateFileError::WrongOwner => {
            LedgerError::InvalidFile("台账及所属状态目录必须归当前用户所有")
        }
        PrivateFileError::NotADirectory | PrivateFileError::DirectorySymlink => {
            LedgerError::InvalidFile("状态目录及 im 子目录必须是真实目录且不得为符号链接")
        }
        PrivateFileError::BroadDirectoryPermissions => {
            LedgerError::InvalidFile("状态目录或 im 子目录权限过宽（须为 0700）")
        }
        PrivateFileError::DirectoryDisappeared => {
            LedgerError::Io("状态目录在台账操作期间消失".into())
        }
        PrivateFileError::FileSymlink | PrivateFileError::NotARegularFile => {
            LedgerError::InvalidFile("台账文件必须是普通文件且不得为符号链接")
        }
        PrivateFileError::BroadFilePermissions => {
            LedgerError::InvalidFile("台账文件权限过宽（须为 0600）")
        }
        PrivateFileError::FileTooLarge { .. } => {
            LedgerError::InvalidFile("台账文件超过 16 MiB 大小上限")
        }
        PrivateFileError::DirectorySyncAfterReplace { source } => LedgerError::Io(format!(
            "替换台账文件后同步目录失败（{}）",
            io_kind_label(source.kind())
        )),
        PrivateFileError::Io { operation, source } => {
            let op_label = match operation {
                PrivateFileOperation::CreateDirectory => "创建状态目录",
                PrivateFileOperation::OpenDirectory | PrivateFileOperation::OpenFile => {
                    "打开台账目录或文件"
                }
                PrivateFileOperation::InspectDirectory | PrivateFileOperation::InspectFile => {
                    "检查台账目录或文件元数据"
                }
                PrivateFileOperation::ReadFile => "读取台账文件",
                PrivateFileOperation::CreateTemporaryFile => "创建临时台账文件",
                PrivateFileOperation::InspectTemporaryFile => "检查临时台账文件元数据",
                PrivateFileOperation::WriteTemporaryFile => "写入临时台账文件",
                PrivateFileOperation::SyncTemporaryFile => "刷盘临时台账文件",
                PrivateFileOperation::ReplaceFile => "原子替换台账文件",
            };
            LedgerError::Io(format!(
                "{op_label}失败（{}）",
                io_kind_label(source.kind())
            ))
        }
    }
}

fn io_kind_label(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => "路径不存在",
        std::io::ErrorKind::PermissionDenied => "权限不足",
        std::io::ErrorKind::NotADirectory => "目标不是目录",
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => "输入无效",
        _ => "系统 I/O 错误",
    }
}

fn open_im_state_dir(layout: &StateLayout) -> Result<PrivateDirectory, LedgerError> {
    let lattice_dir =
        PrivateDirectory::open_or_create(layout.lattice_root()).map_err(map_private_error)?;
    lattice_dir
        .child(OsStr::new(IM_STATE_SUBDIR), true)
        .map_err(map_private_error)?
        .ok_or(LedgerError::InvalidFile("创建 im 状态目录未返回句柄"))
}

struct LedgerInner {
    layout: StateLayout,
    data: LedgerData,
    redelivery_window: chrono::Duration,
    max_bytes: usize,
    latch: LedgerPublicationLatch,
}

#[cfg(test)]
type ReplaceGateTriple = (
    tokio::sync::oneshot::Sender<()>,
    std::sync::mpsc::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
);

#[cfg(test)]
struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

#[cfg(test)]
impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
static TEST_REPLACE_GATES: std::sync::Mutex<BTreeMap<std::path::PathBuf, ReplaceGateTriple>> =
    std::sync::Mutex::new(BTreeMap::new());

#[cfg(test)]
type TransientWriteFault = (InboundId, tokio::sync::oneshot::Sender<()>);

#[cfg(test)]
static TEST_TRANSIENT_WRITE_ERRORS: std::sync::Mutex<
    BTreeMap<std::path::PathBuf, TransientWriteFault>,
> = std::sync::Mutex::new(BTreeMap::new());

#[cfg(test)]
pub(crate) fn arm_replace_gate_for_test(
    lattice_root: std::path::PathBuf,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    TEST_REPLACE_GATES
        .lock()
        .expect("测试台账替换闸门锁不应中毒")
        .insert(lattice_root, (entered_tx, release_rx, finished_tx));
    (entered_rx, release_tx, finished_rx)
}

#[cfg(test)]
pub(crate) fn arm_transient_write_error_for_test(
    lattice_root: std::path::PathBuf,
    target_inbound_id: InboundId,
) -> tokio::sync::oneshot::Receiver<()> {
    let (hit_tx, hit_rx) = tokio::sync::oneshot::channel();
    TEST_TRANSIENT_WRITE_ERRORS
        .lock()
        .expect("测试台账故障集合锁不应中毒")
        .insert(lattice_root, (target_inbound_id, hit_tx));
    hit_rx
}

/// 并发安全的单实例台账句柄：所有持久化更新按顺序执行，仅在磁盘原子替换成功后推进内存快照。
#[derive(Clone)]
pub(crate) struct Ledger {
    inner: Arc<Mutex<LedgerInner>>,
}

impl Ledger {
    /// 打开或初始化台账，并对早于重投窗口的 `Delivered` 条目执行一次启动期滑动窗口清理。
    #[cfg(test)]
    pub(crate) async fn open_and_prune(
        layout: StateLayout,
        now: DateTime<Utc>,
        redelivery_window: chrono::Duration,
    ) -> Result<Self, LedgerError> {
        Self::open_and_prune_with_latch(
            layout,
            now,
            redelivery_window,
            LedgerPublicationLatch::new(),
        )
        .await
    }

    /// 绑定当前网关实例的发布门闩打开台账并执行启动期滑动窗口清理。
    pub(crate) async fn open_and_prune_with_latch(
        layout: StateLayout,
        now: DateTime<Utc>,
        redelivery_window: chrono::Duration,
        latch: LedgerPublicationLatch,
    ) -> Result<Self, LedgerError> {
        Self::open_and_prune_with_limit_and_latch(
            layout,
            now,
            redelivery_window,
            MAX_LEDGER_BYTES,
            latch,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn open_and_prune_with_limit(
        layout: StateLayout,
        now: DateTime<Utc>,
        redelivery_window: chrono::Duration,
        max_bytes: usize,
    ) -> Result<Self, LedgerError> {
        Self::open_and_prune_with_limit_and_latch(
            layout,
            now,
            redelivery_window,
            max_bytes,
            LedgerPublicationLatch::new(),
        )
        .await
    }

    async fn open_and_prune_with_limit_and_latch(
        layout: StateLayout,
        now: DateTime<Utc>,
        redelivery_window: chrono::Duration,
        max_bytes: usize,
        latch: LedgerPublicationLatch,
    ) -> Result<Self, LedgerError> {
        let layout_for_open = layout.clone();
        let latch_for_open = latch.clone();
        let data = tokio::task::spawn_blocking(move || {
            lattice_server::with_component_panic_scope(|| {
                let im_dir = open_im_state_dir(&layout_for_open)?;
                let existing = im_dir
                    .read(OsStr::new(LEDGER_FILE_NAME), Some(max_bytes))
                    .map_err(map_private_error)?;
                let mut data = match existing {
                    None => LedgerData::default(),
                    Some(bytes) => parse_ledger_bytes(&bytes)?,
                };

                if prune_expired_delivered(&mut data, now, redelivery_window) {
                    let bytes = serialize_checked_ledger_data(&data, max_bytes)?;
                    replace_ledger_guarded(&im_dir, &bytes, &latch_for_open)?;
                }
                Ok::<LedgerData, LedgerError>(data)
            })
        })
        .await
        .map_err(|_| LedgerError::Io("启动期台账读取阻塞任务异常中断".into()))??;

        Ok(Self {
            inner: Arc::new(Mutex::new(LedgerInner {
                layout,
                data,
                redelivery_window,
                max_bytes,
                latch,
            })),
        })
    }

    /// 查询指定对话当前绑定的会话身份。
    pub(crate) async fn bound_session(&self, chat: &ChatKey) -> Option<SessionId> {
        let guard = self.inner.lock().await;
        guard.data.bindings.get(chat).copied()
    }

    /// 查询指定入站事件在台账中的现有相位记录。
    pub(crate) async fn inbound_record(&self, id: &InboundId) -> Option<InboundRecord> {
        let guard = self.inner.lock().await;
        guard.data.inbound.get(id).cloned()
    }

    /// 返回当前遗留的全部 `Registered` 与 `Accepted` 条目快照（按 `InboundId` 升序）。
    pub(crate) async fn pending_records(&self) -> Vec<(InboundId, InboundRecord)> {
        let guard = self.inner.lock().await;
        let mut out = Vec::new();
        for (id, record) in &guard.data.inbound {
            if matches!(
                record,
                InboundRecord::Registered { .. } | InboundRecord::Accepted { .. }
            ) {
                out.push((id.clone(), record.clone()));
            }
        }
        out
    }

    /// 持久登记新入站轮次（`Registered`）；当 `bind_session` 为真时在同一次原子替换中写入对话绑定。
    pub(crate) async fn register_turn(
        &self,
        inbound_id: InboundId,
        chat: ChatKey,
        session: SessionId,
        turn: TurnId,
        bind_session: bool,
    ) -> Result<(), LedgerError> {
        self.mutate(move |data| {
            if bind_session {
                data.bindings.insert(chat.clone(), session);
            }
            data.inbound.insert(
                inbound_id,
                InboundRecord::Registered {
                    chat,
                    session,
                    turn,
                },
            );
            Ok(())
        })
        .await
    }

    /// 在同一次原子替换中清除指定对话的会话绑定，并将 `/new` 事件记为 `DeliveredReset`。
    pub(crate) async fn record_reset(
        &self,
        inbound_id: InboundId,
        chat: ChatKey,
        delivered_at: DateTime<Utc>,
    ) -> Result<(), LedgerError> {
        self.mutate(move |data| {
            data.bindings.remove(&chat);
            data.inbound.insert(
                inbound_id,
                InboundRecord::DeliveredReset { chat, delivered_at },
            );
            Ok(())
        })
        .await
    }

    /// 将已登记轮次推进到 `Accepted` 相位。
    pub(crate) async fn mark_accepted(&self, inbound_id: &InboundId) -> Result<(), LedgerError> {
        let key = inbound_id.clone();
        self.mutate(move |data| {
            let Some(current) = data.inbound.get(&key).cloned() else {
                return Err(LedgerError::Corrupt("推进 Accepted 时缺少原入站条目"));
            };
            match current {
                InboundRecord::Registered {
                    chat,
                    session,
                    turn,
                } => {
                    data.inbound.insert(
                        key,
                        InboundRecord::Accepted {
                            chat,
                            session,
                            turn,
                        },
                    );
                    Ok(())
                }
                InboundRecord::Accepted { .. } | InboundRecord::DeliveredTurn { .. } => Ok(()),
                InboundRecord::DeliveredReset { .. } => {
                    Err(LedgerError::Corrupt("重置条目无法推进为 Accepted"))
                }
            }
        })
        .await
    }

    /// 将 `Registered` 或 `Accepted` 轮次推进到 `Delivered` 终态。
    pub(crate) async fn mark_delivered(
        &self,
        inbound_id: &InboundId,
        delivered_at: DateTime<Utc>,
    ) -> Result<(), LedgerError> {
        let key = inbound_id.clone();
        self.mutate(move |data| {
            let Some(current) = data.inbound.get(&key).cloned() else {
                return Err(LedgerError::Corrupt("推进 Delivered 时缺少原入站条目"));
            };
            match current {
                InboundRecord::Registered {
                    chat,
                    session,
                    turn,
                }
                | InboundRecord::Accepted {
                    chat,
                    session,
                    turn,
                } => {
                    data.inbound.insert(
                        key,
                        InboundRecord::DeliveredTurn {
                            chat,
                            session,
                            turn,
                            delivered_at,
                        },
                    );
                    Ok(())
                }
                InboundRecord::DeliveredTurn { .. } | InboundRecord::DeliveredReset { .. } => {
                    Ok(())
                }
            }
        })
        .await
    }

    async fn mutate<F>(&self, apply: F) -> Result<(), LedgerError>
    where
        F: FnOnce(&mut LedgerData) -> Result<(), LedgerError> + Send + 'static,
    {
        let mut guard = self.inner.clone().lock_owned().await;
        let layout = guard.layout.clone();
        let redelivery_window = guard.redelivery_window;
        let max_bytes = guard.max_bytes;
        let latch = guard.latch.clone();
        let mut candidate = guard.data.clone();
        let updated = tokio::task::spawn_blocking(move || {
            lattice_server::with_component_panic_scope(|| {
                apply(&mut candidate)?;
                let bytes = match serialize_checked_ledger_data(&candidate, max_bytes) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        // 长期运行累积接近大小上限时，先清理已过重投窗口的 Delivered 条目再核验大小；
                        // 若仍超限则在原子替换前直接拒绝，绝不写出下次启动无法读取的超限台账文件。
                        prune_expired_delivered(&mut candidate, Utc::now(), redelivery_window);
                        serialize_checked_ledger_data(&candidate, max_bytes)?
                    }
                };
                #[cfg(test)]
                let mut _notify_finished_on_drop = NotifyOnDrop(None);
                #[cfg(test)]
                {
                    let maybe_fault_hit = {
                        let mut map = TEST_TRANSIENT_WRITE_ERRORS
                            .lock()
                            .expect("测试台账故障集合锁不应中毒");
                        if map
                            .get(layout.lattice_root())
                            .is_some_and(|(target_id, _)| candidate.inbound.contains_key(target_id))
                        {
                            map.remove(layout.lattice_root()).map(|(_, tx)| tx)
                        } else {
                            None
                        }
                    };
                    if let Some(hit_tx) = maybe_fault_hit {
                        let _ = hit_tx.send(());
                        return Err(LedgerError::Io("注入的临时台账写入故障".into()));
                    }
                    let maybe_gate = TEST_REPLACE_GATES
                        .lock()
                        .expect("测试台账替换闸门锁不应中毒")
                        .remove(layout.lattice_root());
                    if let Some((entered_tx, release_rx, finished_tx)) = maybe_gate {
                        _notify_finished_on_drop = NotifyOnDrop(Some(finished_tx));
                        let _ = entered_tx.send(());
                        let _ = release_rx.recv();
                    }
                }
                let im_dir = open_im_state_dir(&layout)?;
                replace_ledger_guarded(&im_dir, &bytes, &latch)?;
                Ok::<LedgerData, LedgerError>(candidate)
            })
        })
        .await
        .map_err(|_| LedgerError::Io("台账写入阻塞任务异常中断".into()))??;

        guard.data = updated;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use chrono::TimeZone;

    use super::*;
    use crate::{
        identity::{new_session_id, new_turn_id},
        telegram::TELEGRAM_REDELIVERY_WINDOW,
    };

    #[tokio::test]
    async fn ledger_roundtrips_phases_and_prunes_only_expired_delivered_entries() {
        let dir = tempfile::tempdir().expect("创建临时状态根应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let base_time = Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0).unwrap();

        let ledger = Ledger::open_and_prune(layout.clone(), base_time, TELEGRAM_REDELIVERY_WINDOW)
            .await
            .expect("首次打开缺失台账应视为空台账");

        let chat_a = ChatKey::from_platform_id("tg", "1001");
        let chat_b = ChatKey::from_platform_id("tg", "1002");
        let s_a = new_session_id();
        let s_b = new_session_id();

        let id_old_turn = InboundId::from_platform_id("tg", "1");
        let id_old_reset = InboundId::from_platform_id("tg", "2");
        let id_recent_turn = InboundId::from_platform_id("tg", "3");
        let id_registered = InboundId::from_platform_id("tg", "4");
        let id_accepted = InboundId::from_platform_id("tg", "5");

        let t_old = new_turn_id();
        let t_recent = new_turn_id();
        let t_reg = new_turn_id();
        let t_acc = new_turn_id();

        // 写入并推进到各自相位
        ledger
            .register_turn(id_old_turn.clone(), chat_a.clone(), s_a, t_old, true)
            .await
            .unwrap();
        ledger
            .mark_delivered(&id_old_turn, base_time - chrono::Duration::hours(25))
            .await
            .unwrap();

        ledger
            .record_reset(
                id_old_reset.clone(),
                chat_a.clone(),
                base_time - chrono::Duration::hours(24) - chrono::Duration::minutes(6),
            )
            .await
            .unwrap();
        assert_eq!(
            ledger.bound_session(&chat_a).await,
            None,
            "/new 应在同一次写入中清除 chat_a 绑定"
        );

        // 重新绑定 chat_a 与 chat_b
        ledger
            .register_turn(id_recent_turn.clone(), chat_a.clone(), s_a, t_recent, true)
            .await
            .unwrap();
        ledger
            .mark_delivered(&id_recent_turn, base_time - chrono::Duration::hours(24))
            .await
            .unwrap();

        ledger
            .register_turn(id_registered.clone(), chat_a.clone(), s_a, t_reg, false)
            .await
            .unwrap();
        ledger
            .register_turn(id_accepted.clone(), chat_b.clone(), s_b, t_acc, true)
            .await
            .unwrap();
        ledger.mark_accepted(&id_accepted).await.unwrap();

        // 核验磁盘文件权限为 0600，目录权限为 0700
        let im_dir_path = layout.lattice_root().join("im");
        let ledger_path = im_dir_path.join("ledger");
        assert_eq!(
            std::fs::metadata(&im_dir_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&ledger_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        // 重新打开并触发滑动窗口清理（24h + 5m）
        let reopened =
            Ledger::open_and_prune(layout.clone(), base_time, TELEGRAM_REDELIVERY_WINDOW)
                .await
                .expect("重开台账并清理过期条目应成功");

        assert_eq!(
            reopened.inbound_record(&id_old_turn).await,
            None,
            "超过 24h+5m 的 DeliveredTurn 应被清理"
        );
        assert_eq!(
            reopened.inbound_record(&id_old_reset).await,
            None,
            "超过 24h+5m 的 DeliveredReset 应被清理"
        );
        assert!(
            matches!(
                reopened.inbound_record(&id_recent_turn).await,
                Some(InboundRecord::DeliveredTurn { .. })
            ),
            "24h+5m 窗口内的 Delivered 条目必须保留"
        );
        assert!(
            matches!(
                reopened.inbound_record(&id_registered).await,
                Some(InboundRecord::Registered { .. })
            ),
            "Registered 条目绝不能被窗口清理删除"
        );
        assert!(
            matches!(
                reopened.inbound_record(&id_accepted).await,
                Some(InboundRecord::Accepted { .. })
            ),
            "Accepted 条目绝不能被窗口清理删除"
        );
        assert_eq!(reopened.bound_session(&chat_a).await, Some(s_a));
        assert_eq!(reopened.bound_session(&chat_b).await, Some(s_b));
    }

    #[tokio::test]
    async fn corrupt_or_invalid_ledger_errors_without_overwriting_file() {
        let dir = tempfile::tempdir().expect("创建临时目录应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let now = Utc::now();

        let im_dir = open_im_state_dir(&layout).expect("创建私有目录应成功");
        let sid = new_session_id();
        let tid = new_turn_id();

        let invalid_payloads = [
            ("语法损坏", "{broken".to_owned()),
            (
                "版本号不符",
                r#"{"version":2,"bindings":{},"inbound":{}}"#.to_owned(),
            ),
            (
                "未知顶层字段",
                r#"{"version":1,"bindings":{},"inbound":{},"extra":1}"#.to_owned(),
            ),
            (
                "重复绑定键",
                format!(
                    r#"{{"version":1,"bindings":{{"tg:1":"{sid}","tg:1":"{sid}"}},"inbound":{{}}}}"#
                ),
            ),
            (
                "registered 缺 turn",
                format!(
                    r#"{{"version":1,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"registered","chat":"tg:1","session":"{sid}"}}}}}}"#
                ),
            ),
            (
                "accepted 含 delivered_at",
                format!(
                    r#"{{"version":1,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"accepted","chat":"tg:1","session":"{sid}","turn":"{tid}","delivered_at":"2026-11-14T00:00:00Z"}}}}}}"#
                ),
            ),
            (
                "delivered 只有 session 无 turn",
                format!(
                    r#"{{"version":1,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"delivered","chat":"tg:1","session":"{sid}","delivered_at":"2026-11-14T00:00:00Z"}}}}}}"#
                ),
            ),
        ];

        let ledger_path = layout.lattice_root().join("im/ledger");
        for (label, raw) in invalid_payloads {
            im_dir
                .replace(OsStr::new("ledger"), raw.as_bytes())
                .expect("写入测试载荷应成功");
            let err = Ledger::open_and_prune(layout.clone(), now, TELEGRAM_REDELIVERY_WINDOW)
                .await
                .err()
                .unwrap_or_else(|| panic!("{label} 必须返回损坏错误"));
            assert!(
                matches!(err, LedgerError::Corrupt(_)),
                "{label} 应归类为 Corrupt，实际: {err:?}"
            );
            let on_disk = std::fs::read_to_string(&ledger_path).unwrap();
            assert_eq!(on_disk, raw, "{label} 报错后不得覆盖磁盘原有台账内容");
        }
    }

    #[tokio::test]
    async fn mutate_prunes_expired_delivered_near_cap_and_never_publishes_unreadable_ledger() {
        let dir = tempfile::tempdir().expect("创建临时目录应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let cap_bytes = 900;

        let ledger = Ledger::open_and_prune_with_limit(
            layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
            cap_bytes,
        )
        .await
        .expect("初始化受限台账应成功");

        let chat = ChatKey::from_platform_id("tg", "77");
        let sid = new_session_id();
        let expired_time = Utc::now() - chrono::Duration::hours(30);

        // 先写入若干已过重投窗口的 Delivered 条目，使文件逼近上限
        for idx in 1..=3 {
            let id = InboundId::from_platform_id("tg", &format!("old-{idx}"));
            ledger
                .register_turn(id.clone(), chat.clone(), sid, new_turn_id(), idx == 1)
                .await
                .unwrap();
            ledger.mark_delivered(&id, expired_time).await.unwrap();
        }

        // 继续写入新条目时若超出 cap_bytes，mutate 自动回收过期 Delivered 条目并保持产物可读
        let fresh_id = InboundId::from_platform_id("tg", "fresh-1");
        ledger
            .register_turn(fresh_id.clone(), chat.clone(), sid, new_turn_id(), false)
            .await
            .expect("清理过期 Delivered 后新写入应成功落盘");
        assert_eq!(
            ledger
                .inbound_record(&InboundId::from_platform_id("tg", "old-1"))
                .await,
            None,
            "触发大小上限时应自动清理过期 Delivered 条目"
        );

        // 持续塞入未决 Registered 条目直至无法再清理：超限写入必须在 replace 前失败，磁盘产物仍可重开读取！
        let mut hit_cap = false;
        for idx in 2..=20 {
            let id = InboundId::from_platform_id("tg", &format!("active-{idx}"));
            match ledger
                .register_turn(id, chat.clone(), sid, new_turn_id(), false)
                .await
            {
                Ok(()) => {}
                Err(LedgerError::InvalidFile(_)) => {
                    hit_cap = true;
                    break;
                }
                Err(other) => panic!("超限应返回 InvalidFile，实际: {other:?}"),
            }
        }
        assert!(hit_cap, "未决记录持续累积超过上限时必须拒绝发布");

        let reopened = Ledger::open_and_prune_with_limit(
            layout,
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
            cap_bytes,
        )
        .await
        .expect("拒绝超限写入后磁盘上的台账文件必须仍能被同上限正常读取");
        assert!(
            matches!(
                reopened.inbound_record(&fresh_id).await,
                Some(InboundRecord::Registered { .. })
            ),
            "超限写入失败前的已提交记录必须完好保留"
        );
    }
}
