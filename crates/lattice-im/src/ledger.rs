// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! IM 运行态台账（`<state-root>/lattice/im/ledger`）：维护对话会话绑定、入站事件相位流转与滑动窗口清理。

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fmt,
    sync::Arc,
};

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
const LEDGER_VERSION: u32 = 2;
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
/// 台账只记录身份键、相位与投递完成时间，不保存消息正文、图片字节、回复正文、平台凭据、轮询游标或草稿编号。
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
    /// 多图相册中并入首条轮次的其余分片条目（重投时静默确认，不单独驱动提交或恢复）。
    Merged {
        chat: ChatKey,
        session: SessionId,
        turn: TurnId,
        delivered_at: DateTime<Utc>,
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
            "merged" => match (self.session, self.turn, self.delivered_at) {
                (Some(session), Some(turn), Some(delivered_at)) => Ok(InboundRecord::Merged {
                    chat: self.chat,
                    session,
                    turn,
                    delivered_at,
                }),
                _ => Err(LedgerError::Corrupt(
                    "merged 条目必须同时包含 session、turn 与 delivered_at",
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
            InboundRecord::Merged {
                chat,
                session,
                turn,
                delivered_at,
            } => RawInboundEntryWrite {
                phase: "merged",
                chat,
                session: Some(*session),
                turn: Some(*turn),
                delivered_at: Some(*delivered_at),
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
    let mut active_turns = BTreeSet::new();
    for record in data.inbound.values() {
        if let InboundRecord::Registered { turn, .. } | InboundRecord::Accepted { turn, .. } =
            record
        {
            active_turns.insert(*turn);
        }
    }

    let before_len = data.inbound.len();
    data.inbound.retain(|_, record| match record {
        InboundRecord::DeliveredTurn { delivered_at, .. }
        | InboundRecord::DeliveredReset { delivered_at, .. } => {
            now.signed_duration_since(*delivered_at) <= redelivery_window
        }
        InboundRecord::Merged {
            turn, delivered_at, ..
        } => {
            now.signed_duration_since(*delivered_at) <= redelivery_window
                || active_turns.contains(turn)
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

struct LedgerMeta {
    layout: StateLayout,
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

/// 并发安全的单实例台账句柄：写入经异步互斥锁串行化，仅在磁盘原子替换成功后向内存快照短锁换入新 `Arc<LedgerData>`，读操作仅在短锁内克隆引用计数且不跨越 `.await` 或磁盘 I/O。
#[derive(Clone)]
pub(crate) struct Ledger {
    write_lock: Arc<Mutex<()>>,
    snapshot: Arc<std::sync::Mutex<Arc<LedgerData>>>,
    meta: Arc<LedgerMeta>,
}

impl Ledger {
    /// 打开或初始化台账，并对满足窗口清理条件的终态条目执行一次启动期滑动窗口清理。
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
            write_lock: Arc::new(Mutex::new(())),
            snapshot: Arc::new(std::sync::Mutex::new(Arc::new(data))),
            meta: Arc::new(LedgerMeta {
                layout,
                redelivery_window,
                max_bytes,
                latch,
            }),
        })
    }

    fn current_snapshot(&self) -> Arc<LedgerData> {
        let guard = self.snapshot.lock().expect("台账内存快照锁不应中毒");
        Arc::clone(&guard)
    }

    /// 查询指定对话当前绑定的会话身份。
    pub(crate) async fn bound_session(&self, chat: &ChatKey) -> Option<SessionId> {
        let snapshot = self.current_snapshot();
        snapshot.bindings.get(chat).copied()
    }

    /// 查询指定入站事件在台账中的现有相位记录。
    pub(crate) async fn inbound_record(&self, id: &InboundId) -> Option<InboundRecord> {
        let snapshot = self.current_snapshot();
        snapshot.inbound.get(id).cloned()
    }

    /// 返回当前遗留的全部 `Registered` 与 `Accepted` 条目快照（按 `InboundId` 升序）。
    pub(crate) async fn pending_records(&self) -> Vec<(InboundId, InboundRecord)> {
        let snapshot = self.current_snapshot();
        let mut out = Vec::new();
        for (id, record) in &snapshot.inbound {
            match record {
                InboundRecord::Registered { .. } | InboundRecord::Accepted { .. } => {
                    out.push((id.clone(), record.clone()));
                }
                InboundRecord::Merged { .. }
                | InboundRecord::DeliveredTurn { .. }
                | InboundRecord::DeliveredReset { .. } => {}
            }
        }
        out
    }

    /// 整组原子登记新入站轮次：将首条 `primary` 记为 `Registered`，其余分片 `merged` 记为 `Merged`；
    /// 当 `bind_session` 为真时在同一次原子替换中写入对话绑定。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn register_turn(
        &self,
        primary: InboundId,
        merged: Vec<InboundId>,
        chat: ChatKey,
        session: SessionId,
        turn: TurnId,
        bind_session: bool,
        delivered_at: DateTime<Utc>,
    ) -> Result<(), LedgerError> {
        self.mutate(move |data| {
            let mut seen = BTreeSet::new();
            seen.insert(&primary);
            for id in &merged {
                if !seen.insert(id) {
                    return Err(LedgerError::Corrupt("整组登记包含重复的入站事件标识"));
                }
            }
            drop(seen);

            if data.inbound.contains_key(&primary)
                || merged.iter().any(|id| data.inbound.contains_key(id))
            {
                return Err(LedgerError::Corrupt(
                    "整组登记包含台账中已存在的入站事件标识",
                ));
            }

            if bind_session {
                data.bindings.insert(chat.clone(), session);
            }
            data.inbound.insert(
                primary,
                InboundRecord::Registered {
                    chat: chat.clone(),
                    session,
                    turn,
                },
            );
            for merged_id in merged {
                data.inbound.insert(
                    merged_id,
                    InboundRecord::Merged {
                        chat: chat.clone(),
                        session,
                        turn,
                        delivered_at,
                    },
                );
            }
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
                InboundRecord::Merged { .. } => {
                    Err(LedgerError::Corrupt("已合并条目无法推进为 Accepted"))
                }
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
                InboundRecord::Merged { .. }
                | InboundRecord::DeliveredTurn { .. }
                | InboundRecord::DeliveredReset { .. } => Ok(()),
            }
        })
        .await
    }

    async fn mutate<F>(&self, apply: F) -> Result<(), LedgerError>
    where
        F: FnOnce(&mut LedgerData) -> Result<(), LedgerError> + Send + 'static,
    {
        let write_guard = Arc::clone(&self.write_lock).lock_owned().await;
        let layout = self.meta.layout.clone();
        let redelivery_window = self.meta.redelivery_window;
        let max_bytes = self.meta.max_bytes;
        let latch = self.meta.latch.clone();
        let snapshot = Arc::clone(&self.snapshot);
        let base_snapshot = self.current_snapshot();
        tokio::task::spawn_blocking(move || {
            let _write_guard = write_guard;
            lattice_server::with_component_panic_scope(|| {
                let mut candidate = (*base_snapshot).clone();
                drop(base_snapshot);
                apply(&mut candidate)?;
                let bytes = match serialize_checked_ledger_data(&candidate, max_bytes) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        // 长期运行累积接近大小上限时，先清理已过重投窗口的可回收条目再核验大小；
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
                let next_snapshot = Arc::new(candidate);
                let prev_snapshot = {
                    let mut guard = snapshot.lock().expect("台账内存快照锁不应中毒");
                    std::mem::replace(&mut *guard, next_snapshot)
                };
                drop(prev_snapshot);
                Ok::<(), LedgerError>(())
            })
        })
        .await
        .map_err(|_| LedgerError::Io("台账写入阻塞任务异常中断".into()))??;

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
            .register_turn(
                id_old_turn.clone(),
                Vec::new(),
                chat_a.clone(),
                s_a,
                t_old,
                true,
                base_time,
            )
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
            .register_turn(
                id_recent_turn.clone(),
                Vec::new(),
                chat_a.clone(),
                s_a,
                t_recent,
                true,
                base_time,
            )
            .await
            .unwrap();
        ledger
            .mark_delivered(&id_recent_turn, base_time - chrono::Duration::hours(24))
            .await
            .unwrap();

        ledger
            .register_turn(
                id_registered.clone(),
                Vec::new(),
                chat_a.clone(),
                s_a,
                t_reg,
                false,
                base_time,
            )
            .await
            .unwrap();
        ledger
            .register_turn(
                id_accepted.clone(),
                Vec::new(),
                chat_b.clone(),
                s_b,
                t_acc,
                true,
                base_time,
            )
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

        let ledger_path = layout.lattice_root().join("im/ledger");

        // 合法的 v1 台账（空台账与含绑定及条目的台账）必须因版本号不受支持被拒绝，且绝不覆盖原文件
        let v1_payloads = [
            (
                "空 v1 台账",
                r#"{"version":1,"bindings":{},"inbound":{}}"#.to_owned(),
            ),
            (
                "含条目合法 v1 台账",
                format!(
                    r#"{{"version":1,"bindings":{{"tg:1":"{sid}"}},"inbound":{{"tg:1":{{"phase":"delivered","chat":"tg:1","session":"{sid}","turn":"{tid}","delivered_at":"2026-11-14T00:00:00Z"}}}}}}"#
                ),
            ),
        ];
        for (label, raw) in v1_payloads {
            im_dir
                .replace(OsStr::new("ledger"), raw.as_bytes())
                .expect("写入 v1 测试载荷应成功");
            let err = Ledger::open_and_prune(layout.clone(), now, TELEGRAM_REDELIVERY_WINDOW)
                .await
                .err()
                .unwrap_or_else(|| panic!("{label} 必须返回版本号不受支持错误"));
            assert_eq!(
                err,
                LedgerError::Corrupt("版本号不受支持"),
                "{label} 错误内容必须精确匹配版本号不受支持"
            );
            let on_disk = std::fs::read_to_string(&ledger_path).unwrap();
            assert_eq!(on_disk, raw, "{label} 报错后不得覆盖磁盘原有台账内容");
        }

        let invalid_payloads = [
            ("语法损坏", "{broken".to_owned()),
            (
                "未知顶层字段",
                r#"{"version":2,"bindings":{},"inbound":{},"extra":1}"#.to_owned(),
            ),
            (
                "末尾多余尾随字节",
                r#"{"version":2,"bindings":{},"inbound":{}} trailing"#.to_owned(),
            ),
            (
                "重复绑定键",
                format!(
                    r#"{{"version":2,"bindings":{{"tg:1":"{sid}","tg:1":"{sid}"}},"inbound":{{}}}}"#
                ),
            ),
            (
                "registered 缺 turn",
                format!(
                    r#"{{"version":2,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"registered","chat":"tg:1","session":"{sid}"}}}}}}"#
                ),
            ),
            (
                "accepted 含 delivered_at",
                format!(
                    r#"{{"version":2,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"accepted","chat":"tg:1","session":"{sid}","turn":"{tid}","delivered_at":"2026-11-14T00:00:00Z"}}}}}}"#
                ),
            ),
            (
                "delivered 只有 session 无 turn",
                format!(
                    r#"{{"version":2,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"delivered","chat":"tg:1","session":"{sid}","delivered_at":"2026-11-14T00:00:00Z"}}}}}}"#
                ),
            ),
            (
                "merged 缺 session",
                format!(
                    r#"{{"version":2,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"merged","chat":"tg:1","turn":"{tid}","delivered_at":"2026-11-14T00:00:00Z"}}}}}}"#
                ),
            ),
            (
                "merged 缺 turn",
                format!(
                    r#"{{"version":2,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"merged","chat":"tg:1","session":"{sid}","delivered_at":"2026-11-14T00:00:00Z"}}}}}}"#
                ),
            ),
            (
                "merged 缺 delivered_at",
                format!(
                    r#"{{"version":2,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"merged","chat":"tg:1","session":"{sid}","turn":"{tid}"}}}}}}"#
                ),
            ),
            (
                "merged 含未知字段",
                format!(
                    r#"{{"version":2,"bindings":{{}},"inbound":{{"tg:1":{{"phase":"merged","chat":"tg:1","session":"{sid}","turn":"{tid}","delivered_at":"2026-11-14T00:00:00Z","extra":true}}}}}}"#
                ),
            ),
        ];

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
                .register_turn(
                    id.clone(),
                    Vec::new(),
                    chat.clone(),
                    sid,
                    new_turn_id(),
                    idx == 1,
                    expired_time,
                )
                .await
                .unwrap();
            ledger.mark_delivered(&id, expired_time).await.unwrap();
        }

        // 继续写入新条目时若超出 cap_bytes，mutate 自动回收过期 Delivered 条目并保持产物可读
        let fresh_id = InboundId::from_platform_id("tg", "fresh-1");
        ledger
            .register_turn(
                fresh_id.clone(),
                Vec::new(),
                chat.clone(),
                sid,
                new_turn_id(),
                false,
                Utc::now(),
            )
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
                .register_turn(
                    id,
                    Vec::new(),
                    chat.clone(),
                    sid,
                    new_turn_id(),
                    false,
                    Utc::now(),
                )
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

    #[tokio::test]
    async fn register_turn_roundtrips_album_group_skips_merged_in_pending_and_enforces_phase_rules()
    {
        let dir = tempfile::tempdir().expect("创建临时状态根应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let base_time = Utc.with_ymd_and_hms(2026, 11, 14, 15, 0, 0).unwrap();

        let ledger = Ledger::open_and_prune(layout.clone(), base_time, TELEGRAM_REDELIVERY_WINDOW)
            .await
            .expect("初始化空台账应成功");

        let chat_album = ChatKey::from_platform_id("tg", "2001");
        let chat_single = ChatKey::from_platform_id("tg", "2002");
        let s_album = new_session_id();
        let s_single = new_session_id();
        let t_album = new_turn_id();
        let t_single = new_turn_id();

        let p_album = InboundId::from_platform_id("tg", "101");
        let m_album_1 = InboundId::from_platform_id("tg", "102");
        let m_album_2 = InboundId::from_platform_id("tg", "103");
        let id_single = InboundId::from_platform_id("tg", "104");
        let id_reset = InboundId::from_platform_id("tg", "105");

        // 1. 校验重复 InboundId 被拒绝且不留下半绑定或半组记录
        let dup_with_primary = ledger
            .register_turn(
                p_album.clone(),
                vec![m_album_1.clone(), p_album.clone()],
                chat_album.clone(),
                s_album,
                t_album,
                true,
                base_time,
            )
            .await
            .expect_err("merged 含首条相同标识时必须拒绝");
        assert!(matches!(dup_with_primary, LedgerError::Corrupt(_)));
        assert_eq!(ledger.bound_session(&chat_album).await, None);
        assert_eq!(ledger.inbound_record(&p_album).await, None);
        assert_eq!(ledger.inbound_record(&m_album_1).await, None);

        let dup_in_merged = ledger
            .register_turn(
                p_album.clone(),
                vec![m_album_1.clone(), m_album_1.clone()],
                chat_album.clone(),
                s_album,
                t_album,
                true,
                base_time,
            )
            .await
            .expect_err("merged 内部包含重复标识时必须拒绝");
        assert!(matches!(dup_in_merged, LedgerError::Corrupt(_)));
        assert_eq!(ledger.bound_session(&chat_album).await, None);

        // 2. 整组原子登记（1 条 Registered + 2 条 Merged + 会话绑定）与单条消息登记
        ledger
            .register_turn(
                p_album.clone(),
                vec![m_album_1.clone(), m_album_2.clone()],
                chat_album.clone(),
                s_album,
                t_album,
                true,
                base_time,
            )
            .await
            .expect("相册整组原子登记应成功");

        ledger
            .register_turn(
                id_single.clone(),
                Vec::new(),
                chat_single.clone(),
                s_single,
                t_single,
                true,
                base_time,
            )
            .await
            .expect("单条消息登记应成功");
        ledger
            .mark_delivered(&id_single, base_time)
            .await
            .expect("推进单条消息至 DeliveredTurn 应成功");
        ledger
            .record_reset(
                id_reset.clone(),
                ChatKey::from_platform_id("tg", "2003"),
                base_time,
            )
            .await
            .expect("登记重置条目应成功");

        // 核验磁盘原始 JSON 为 v2 且包含 phase: "merged"
        let raw_json = std::fs::read_to_string(layout.lattice_root().join("im/ledger")).unwrap();
        assert!(
            raw_json.contains("\"version\": 2"),
            "落盘版本号必须为 2: {raw_json}"
        );
        assert!(
            raw_json.contains("\"phase\": \"merged\""),
            "落盘条目必须使用 merged 相位标签: {raw_json}"
        );

        // 3. 重开往返，核验绑定、各条目相位以及 pending_records 仅返回首条 Registered
        let reopened =
            Ledger::open_and_prune(layout.clone(), base_time, TELEGRAM_REDELIVERY_WINDOW)
                .await
                .expect("重开 v2 台账应成功");

        assert_eq!(reopened.bound_session(&chat_album).await, Some(s_album));
        assert_eq!(reopened.bound_session(&chat_single).await, Some(s_single));
        assert_eq!(
            reopened.inbound_record(&p_album).await,
            Some(InboundRecord::Registered {
                chat: chat_album.clone(),
                session: s_album,
                turn: t_album,
            })
        );
        for mid in [&m_album_1, &m_album_2] {
            assert_eq!(
                reopened.inbound_record(mid).await,
                Some(InboundRecord::Merged {
                    chat: chat_album.clone(),
                    session: s_album,
                    turn: t_album,
                    delivered_at: base_time,
                })
            );
        }

        let pending = reopened.pending_records().await;
        assert_eq!(
            pending,
            vec![(
                p_album.clone(),
                InboundRecord::Registered {
                    chat: chat_album.clone(),
                    session: s_album,
                    turn: t_album,
                }
            )],
            "pending_records 只能返回首条 Registered（跳过 Merged、DeliveredTurn、DeliveredReset）"
        );

        // 4. Merged 相位流转约束：mark_accepted 报错，mark_delivered 幂等返回 Ok(())
        let accept_err = reopened
            .mark_accepted(&m_album_1)
            .await
            .expect_err("Merged 条目不得推进为 Accepted");
        assert_eq!(
            accept_err,
            LedgerError::Corrupt("已合并条目无法推进为 Accepted")
        );
        reopened
            .mark_delivered(&m_album_1, base_time + chrono::Duration::minutes(10))
            .await
            .expect("Merged 条目调用 mark_delivered 应按终态幂等成功");
        assert_eq!(
            reopened.inbound_record(&m_album_1).await,
            Some(InboundRecord::Merged {
                chat: chat_album,
                session: s_album,
                turn: t_album,
                delivered_at: base_time,
            }),
            "mark_delivered 幂等返回后应保持原 Merged 记录不变"
        );
    }

    #[tokio::test]
    async fn register_turn_rejects_overwriting_existing_inbound_entries_preserving_snapshot_and_disk()
     {
        let dir = tempfile::tempdir().expect("创建临时状态根应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let base_time = Utc.with_ymd_and_hms(2026, 11, 14, 15, 30, 0).unwrap();

        let ledger = Ledger::open_and_prune(layout.clone(), base_time, TELEGRAM_REDELIVERY_WINDOW)
            .await
            .expect("初始化空台账应成功");

        let chat_orig = ChatKey::from_platform_id("tg", "2101");
        let chat_other = ChatKey::from_platform_id("tg", "2102");
        let sid_orig = new_session_id();
        let sid_intruder = new_session_id();

        let id_reg = InboundId::from_platform_id("tg", "r-1");
        let id_merged = InboundId::from_platform_id("tg", "r-2");
        let id_acc = InboundId::from_platform_id("tg", "r-3");
        let id_del = InboundId::from_platform_id("tg", "r-4");

        let t_reg = new_turn_id();
        let t_acc = new_turn_id();
        let t_del = new_turn_id();

        // 构造覆盖 Registered、Merged、Accepted、DeliveredTurn 四种已存在条目
        ledger
            .register_turn(
                id_reg.clone(),
                vec![id_merged.clone()],
                chat_orig.clone(),
                sid_orig,
                t_reg,
                true,
                base_time,
            )
            .await
            .unwrap();
        ledger
            .register_turn(
                id_acc.clone(),
                Vec::new(),
                chat_orig.clone(),
                sid_orig,
                t_acc,
                false,
                base_time,
            )
            .await
            .unwrap();
        ledger.mark_accepted(&id_acc).await.unwrap();
        ledger
            .register_turn(
                id_del.clone(),
                Vec::new(),
                chat_orig.clone(),
                sid_orig,
                t_del,
                false,
                base_time,
            )
            .await
            .unwrap();
        ledger.mark_delivered(&id_del, base_time).await.unwrap();

        let ledger_path = layout.lattice_root().join("im/ledger");
        let disk_before = std::fs::read(&ledger_path).expect("读取初始台账字节应成功");

        for (phase_label, existing_id) in [
            ("Registered", &id_reg),
            ("Accepted", &id_acc),
            ("Merged", &id_merged),
            ("DeliveredTurn", &id_del),
        ] {
            let expected_record = ledger
                .inbound_record(existing_id)
                .await
                .expect("既有条目应存在");
            let fresh_companion =
                InboundId::from_platform_id("tg", &format!("fresh-{phase_label}"));

            // (a) 既有 ID 作为 primary 传入 register_turn（同时尝试篡改已有绑定或绑定新对话）
            let err_as_primary = ledger
                .register_turn(
                    existing_id.clone(),
                    vec![fresh_companion.clone()],
                    chat_orig.clone(),
                    sid_intruder,
                    new_turn_id(),
                    true,
                    base_time + chrono::Duration::minutes(5),
                )
                .await
                .expect_err("已存在入站条目作为 primary 时必须拒绝覆盖");
            assert_eq!(
                err_as_primary,
                LedgerError::Corrupt("整组登记包含台账中已存在的入站事件标识"),
                "{phase_label} 作为 primary 冲突时错误信息必须匹配"
            );

            // (b) 既有 ID 混入 merged 传入 register_turn
            let err_in_merged = ledger
                .register_turn(
                    fresh_companion.clone(),
                    vec![existing_id.clone()],
                    chat_other.clone(),
                    sid_intruder,
                    new_turn_id(),
                    true,
                    base_time + chrono::Duration::minutes(5),
                )
                .await
                .expect_err("已存在入站条目出现在 merged 中时必须拒绝覆盖");
            assert_eq!(
                err_in_merged,
                LedgerError::Corrupt("整组登记包含台账中已存在的入站事件标识"),
                "{phase_label} 出现在 merged 冲突时错误信息必须匹配"
            );

            assert_eq!(
                ledger.bound_session(&chat_orig).await,
                Some(sid_orig),
                "{phase_label} 拒绝覆盖后原有会话绑定必须保持不变"
            );
            assert_eq!(
                ledger.bound_session(&chat_other).await,
                None,
                "{phase_label} 拒绝覆盖后不得写入新对话绑定"
            );
            assert_eq!(
                ledger.inbound_record(existing_id).await,
                Some(expected_record),
                "{phase_label} 拒绝覆盖后原有入站记录必须保持原样"
            );
            assert_eq!(
                ledger.inbound_record(&fresh_companion).await,
                None,
                "{phase_label} 拒绝覆盖后同批新 ID 不得残留于内存中"
            );
            assert_eq!(
                std::fs::read(&ledger_path).unwrap(),
                disk_before,
                "{phase_label} 拒绝覆盖后磁盘台账文件必须保持原样"
            );
        }
    }

    #[tokio::test]
    async fn album_register_turn_rolls_back_atomically_on_transient_fault_closed_latch_or_cap_overflow()
     {
        let dir = tempfile::tempdir().expect("创建临时状态根应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let now = Utc.with_ymd_and_hms(2026, 11, 14, 16, 0, 0).unwrap();

        let ledger = Ledger::open_and_prune(layout.clone(), now, TELEGRAM_REDELIVERY_WINDOW)
            .await
            .expect("初始化默认上限台账应成功");

        let chat = ChatKey::from_platform_id("tg", "3001");
        let sid = new_session_id();
        let tid = new_turn_id();
        let p_id = InboundId::from_platform_id("tg", "201");
        let m_id_1 = InboundId::from_platform_id("tg", "202");
        let m_id_2 = InboundId::from_platform_id("tg", "203");

        // 1. 针对整组中的 Merged 成员注入单次临时写故障：必须整组回滚（无首条、无 Merged、无会话绑定）
        let hit_rx =
            arm_transient_write_error_for_test(layout.lattice_root().to_path_buf(), m_id_2.clone());
        let io_err = ledger
            .register_turn(
                p_id.clone(),
                vec![m_id_1.clone(), m_id_2.clone()],
                chat.clone(),
                sid,
                tid,
                true,
                now,
            )
            .await
            .expect_err("注入临时写入故障时整组登记必须返回错误");
        hit_rx.await.expect("故障注入通道应被触发");
        assert!(matches!(io_err, LedgerError::Io(_)));
        assert_eq!(
            ledger.bound_session(&chat).await,
            None,
            "写故障后内存中不得残留半绑定"
        );
        for id in [&p_id, &m_id_1, &m_id_2] {
            assert_eq!(
                ledger.inbound_record(id).await,
                None,
                "写故障后内存中不得残留半组条目: {id}"
            );
        }

        // 2. 整组体积超出上限（cap_bytes = 450，而 1 条 Registered + 2 条 Merged 序列化后超过 600 字节）：必须整组回滚
        let ledger_capped =
            Ledger::open_and_prune_with_limit(layout.clone(), now, TELEGRAM_REDELIVERY_WINDOW, 450)
                .await
                .expect("初始化受限上限台账应成功");
        let cap_err = ledger_capped
            .register_turn(
                p_id.clone(),
                vec![m_id_1.clone(), m_id_2.clone()],
                chat.clone(),
                sid,
                tid,
                true,
                now,
            )
            .await
            .expect_err("整组序列化超出大小上限时必须拒绝落盘");
        assert!(matches!(cap_err, LedgerError::InvalidFile(_)));
        assert_eq!(ledger_capped.bound_session(&chat).await, None);
        for id in [&p_id, &m_id_1, &m_id_2] {
            assert_eq!(ledger_capped.inbound_record(id).await, None);
        }

        // 3. 发布门闩关闭时（体积合法的单条加单分片或小体积测试）：放宽上限验证门闩拦截同样无半组残留
        let latch2 = LedgerPublicationLatch::new();
        let ledger2 = Ledger::open_and_prune_with_latch(
            layout.clone(),
            now,
            TELEGRAM_REDELIVERY_WINDOW,
            latch2.clone(),
        )
        .await
        .expect("重开默认上限台账应成功");
        latch2.close();
        let closed_err = ledger2
            .register_turn(
                p_id.clone(),
                vec![m_id_1.clone()],
                chat.clone(),
                sid,
                tid,
                true,
                now,
            )
            .await
            .expect_err("门闩关闭后整组登记必须返回 PublicationClosed");
        assert_eq!(closed_err, LedgerError::PublicationClosed);
        assert_eq!(ledger2.bound_session(&chat).await, None);
        assert_eq!(ledger2.inbound_record(&p_id).await, None);
        assert_eq!(ledger2.inbound_record(&m_id_1).await, None);

        // 重开磁盘复核：自始至终未写出任何半组状态
        let reopened = Ledger::open_and_prune(layout, now, TELEGRAM_REDELIVERY_WINDOW)
            .await
            .expect("重开磁盘台账应成功");
        assert_eq!(reopened.bound_session(&chat).await, None);
        for id in [&p_id, &m_id_1, &m_id_2] {
            assert_eq!(reopened.inbound_record(id).await, None);
        }
    }

    #[tokio::test]
    async fn concurrent_writes_preserve_all_records_and_reads_never_block_on_replace_gate() {
        let dir = tempfile::tempdir().expect("创建临时状态根应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let now = Utc.with_ymd_and_hms(2026, 11, 14, 17, 0, 0).unwrap();

        let ledger = Ledger::open_and_prune(layout.clone(), now, TELEGRAM_REDELIVERY_WINDOW)
            .await
            .expect("初始化台账应成功");

        let chat_seed = ChatKey::from_platform_id("tg", "4000");
        let sid_seed = new_session_id();
        let id_seed = InboundId::from_platform_id("tg", "seed-0");
        ledger
            .register_turn(
                id_seed.clone(),
                Vec::new(),
                chat_seed.clone(),
                sid_seed,
                new_turn_id(),
                true,
                now,
            )
            .await
            .expect("写入初始已提交记录应成功");

        // 武装磁盘替换闸门，启动第 1 个写任务并确认其阻塞在 spawn_blocking 内的替换闸门处
        let (entered_rx, release_tx, _finished_rx) =
            arm_replace_gate_for_test(layout.lattice_root().to_path_buf());

        let chat_w1 = ChatKey::from_platform_id("tg", "4001");
        let sid_w1 = new_session_id();
        let tid_w1 = new_turn_id();
        let id_w1_p = InboundId::from_platform_id("tg", "w1-p");
        let id_w1_m = InboundId::from_platform_id("tg", "w1-m");

        let ledger_w1 = ledger.clone();
        let chat_w1_clone = chat_w1.clone();
        let id_w1_p_clone = id_w1_p.clone();
        let id_w1_m_clone = id_w1_m.clone();
        let w1_handle = tokio::spawn(async move {
            ledger_w1
                .register_turn(
                    id_w1_p_clone,
                    vec![id_w1_m_clone],
                    chat_w1_clone,
                    sid_w1,
                    tid_w1,
                    true,
                    now,
                )
                .await
        });

        entered_rx
            .await
            .expect("第 1 个写任务应进入 TEST_REPLACE_GATES 阻塞点");

        // 在第 1 个写任务阻塞期间，同时派生多个并发写任务排队等待写锁
        let mut concurrent_handles = Vec::new();
        let mut expected_ids = Vec::new();
        for idx in 2..=6 {
            let l = ledger.clone();
            let c = ChatKey::from_platform_id("tg", &format!("400{idx}"));
            let s = new_session_id();
            let t = new_turn_id();
            let p = InboundId::from_platform_id("tg", &format!("w{idx}-p"));
            let m = InboundId::from_platform_id("tg", &format!("w{idx}-m"));
            expected_ids.push((c.clone(), s, p.clone(), m.clone()));
            concurrent_handles.push(tokio::spawn(async move {
                l.register_turn(p, vec![m], c, s, t, true, now).await
            }));
        }
        tokio::task::yield_now().await;

        // 核验在 TEST_REPLACE_GATES 阻塞落盘期间，inbound_record / bound_session / pending_records 均立即返回已提交快照而不卡死
        let read_timeout = std::time::Duration::from_millis(200);
        let seed_rec = tokio::time::timeout(read_timeout, ledger.inbound_record(&id_seed))
            .await
            .expect("写盘阻塞期间读取已提交条目不得超时");
        assert!(matches!(seed_rec, Some(InboundRecord::Registered { .. })));

        let uncommitted_p = tokio::time::timeout(read_timeout, ledger.inbound_record(&id_w1_p))
            .await
            .expect("写盘阻塞期间查询在途未提交首条不得超时");
        assert_eq!(
            uncommitted_p, None,
            "磁盘原子替换完成前，在途首条记录不得提前暴露于快照中"
        );

        let uncommitted_m = tokio::time::timeout(read_timeout, ledger.inbound_record(&id_w1_m))
            .await
            .expect("写盘阻塞期间查询在途未提交合并条目不得超时");
        assert_eq!(uncommitted_m, None);

        let seed_bound = tokio::time::timeout(read_timeout, ledger.bound_session(&chat_seed))
            .await
            .expect("写盘阻塞期间读取已提交绑定不得超时");
        assert_eq!(seed_bound, Some(sid_seed));

        let uncommitted_bound = tokio::time::timeout(read_timeout, ledger.bound_session(&chat_w1))
            .await
            .expect("写盘阻塞期间读取未提交绑定不得超时");
        assert_eq!(uncommitted_bound, None);

        let pending_during_gate = tokio::time::timeout(read_timeout, ledger.pending_records())
            .await
            .expect("写盘阻塞期间读取 pending_records 不得超时");
        assert_eq!(pending_during_gate.len(), 1);

        // 放行替换闸门：第 1 个写任务完成后立即能读到新记录，后续并发写任务按序完成且不丢失任何条目
        release_tx.send(()).expect("放行替换闸门应成功");
        w1_handle
            .await
            .expect("第 1 个写任务不应 panic")
            .expect("第 1 个写任务落盘应成功");

        assert!(matches!(
            ledger.inbound_record(&id_w1_p).await,
            Some(InboundRecord::Registered { .. })
        ));
        assert!(matches!(
            ledger.inbound_record(&id_w1_m).await,
            Some(InboundRecord::Merged { .. })
        ));
        assert_eq!(ledger.bound_session(&chat_w1).await, Some(sid_w1));

        for handle in concurrent_handles {
            handle
                .await
                .expect("并发写任务不应 panic")
                .expect("并发写任务落盘应成功");
        }

        // 核对内存快照与重开后的磁盘快照均完整保留全部并发写入记录
        let reopened = Ledger::open_and_prune(layout, now, TELEGRAM_REDELIVERY_WINDOW)
            .await
            .expect("并发写入完成后重开台账应成功");
        assert_eq!(reopened.bound_session(&chat_seed).await, Some(sid_seed));
        assert_eq!(reopened.bound_session(&chat_w1).await, Some(sid_w1));
        assert!(reopened.inbound_record(&id_seed).await.is_some());
        assert!(reopened.inbound_record(&id_w1_p).await.is_some());
        assert!(reopened.inbound_record(&id_w1_m).await.is_some());
        for (chat, sid, p, m) in expected_ids {
            assert_eq!(
                ledger.bound_session(&chat).await,
                Some(sid),
                "内存快照不得丢失并发绑定 {chat}"
            );
            assert_eq!(
                reopened.bound_session(&chat).await,
                Some(sid),
                "磁盘重开不得丢失并发绑定 {chat}"
            );
            assert!(
                matches!(
                    reopened.inbound_record(&p).await,
                    Some(InboundRecord::Registered { .. })
                ),
                "磁盘重开不得丢失并发首条记录 {p}"
            );
            assert!(
                matches!(
                    reopened.inbound_record(&m).await,
                    Some(InboundRecord::Merged { .. })
                ),
                "磁盘重开不得丢失并发合并记录 {m}"
            );
        }
    }

    #[tokio::test]
    async fn merged_entries_obey_exact_window_and_active_turn_retention_on_startup_and_near_cap_prune()
     {
        let dir = tempfile::tempdir().expect("创建临时状态根应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let base_time = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        let exact_boundary = base_time - TELEGRAM_REDELIVERY_WINDOW;
        let expired_by_1s = exact_boundary - chrono::Duration::seconds(1);

        let ledger = Ledger::open_and_prune(
            layout.clone(),
            base_time - chrono::Duration::hours(30),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .expect("初始化台账应成功");

        let chat = ChatKey::from_platform_id("tg", "5001");
        let sid = new_session_id();

        // 情形 1：Merged 年龄恰好 == 24h + 5m（且首条已转为过期 DeliveredTurn，不在 active_turns 中）-> 必须保留
        let t_exact = new_turn_id();
        let p_exact = InboundId::from_platform_id("tg", "501");
        let m_exact = InboundId::from_platform_id("tg", "502");
        ledger
            .register_turn(
                p_exact.clone(),
                vec![m_exact.clone()],
                chat.clone(),
                sid,
                t_exact,
                true,
                exact_boundary,
            )
            .await
            .unwrap();
        ledger
            .mark_delivered(&p_exact, expired_by_1s)
            .await
            .unwrap();

        // 情形 2：Merged 超过 24h + 5m，但关联 turn 仍处于 Registered -> 必须保留
        let t_reg = new_turn_id();
        let p_reg = InboundId::from_platform_id("tg", "503");
        let m_reg = InboundId::from_platform_id("tg", "504");
        ledger
            .register_turn(
                p_reg.clone(),
                vec![m_reg.clone()],
                chat.clone(),
                sid,
                t_reg,
                false,
                expired_by_1s,
            )
            .await
            .unwrap();

        // 情形 3：Merged 超过 24h + 5m，但关联 turn 仍处于 Accepted -> 必须保留
        let t_acc = new_turn_id();
        let p_acc = InboundId::from_platform_id("tg", "505");
        let m_acc = InboundId::from_platform_id("tg", "506");
        ledger
            .register_turn(
                p_acc.clone(),
                vec![m_acc.clone()],
                chat.clone(),
                sid,
                t_acc,
                false,
                expired_by_1s,
            )
            .await
            .unwrap();
        ledger.mark_accepted(&p_acc).await.unwrap();

        // 情形 4：Merged 超过 24h + 5m，且关联 turn 已转为 DeliveredTurn -> 启动期清理必须删除
        let t_done = new_turn_id();
        let p_done = InboundId::from_platform_id("tg", "507");
        let m_done = InboundId::from_platform_id("tg", "508");
        ledger
            .register_turn(
                p_done.clone(),
                vec![m_done.clone()],
                chat.clone(),
                sid,
                t_done,
                false,
                expired_by_1s,
            )
            .await
            .unwrap();
        ledger
            .mark_delivered(&p_done, base_time - chrono::Duration::minutes(1))
            .await
            .unwrap();

        let reopened =
            Ledger::open_and_prune(layout.clone(), base_time, TELEGRAM_REDELIVERY_WINDOW)
                .await
                .expect("重开并执行启动期窗口清理应成功");

        assert_eq!(
            reopened.inbound_record(&p_exact).await,
            None,
            "超过窗口的首条 DeliveredTurn 应被清理"
        );
        assert!(
            matches!(
                reopened.inbound_record(&m_exact).await,
                Some(InboundRecord::Merged { .. })
            ),
            "年龄恰好等于 24h+5m 的 Merged 条目必须保留"
        );
        assert!(
            matches!(
                reopened.inbound_record(&m_reg).await,
                Some(InboundRecord::Merged { .. })
            ),
            "超过 24h+5m 但关联 turn 仍为 Registered 的 Merged 条目必须保留"
        );
        assert!(
            matches!(
                reopened.inbound_record(&m_acc).await,
                Some(InboundRecord::Merged { .. })
            ),
            "超过 24h+5m 但关联 turn 仍为 Accepted 的 Merged 条目必须保留"
        );
        assert!(
            matches!(
                reopened.inbound_record(&p_done).await,
                Some(InboundRecord::DeliveredTurn { .. })
            ),
            "窗口内的首条 DeliveredTurn 应保留"
        );
        assert_eq!(
            reopened.inbound_record(&m_done).await,
            None,
            "超过 24h+5m 且关联 turn 已转 DeliveredTurn 的 Merged 条目应在启动清理中被删除"
        );

        // 验证逼近大小上限的写前清理（mutate 内部）：
        // 仅删除超过 24h+5m 且关联 turn 已转 DeliveredTurn 的 Merged，保留关联 turn 仍在途的过期 Merged
        let dir_cap = tempfile::tempdir().expect("创建受限台账临时目录应成功");
        let layout_cap = StateLayout::in_state_root(dir_cap.path());
        let cap_bytes = 1250;
        let ledger_cap = Ledger::open_and_prune_with_limit(
            layout_cap,
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
            cap_bytes,
        )
        .await
        .expect("初始化受限台账应成功");

        let old_wall = Utc::now() - chrono::Duration::hours(30);
        let p_cap_done = InboundId::from_platform_id("tg", "601");
        let m_cap_done = InboundId::from_platform_id("tg", "602");
        ledger_cap
            .register_turn(
                p_cap_done.clone(),
                vec![m_cap_done.clone()],
                chat.clone(),
                sid,
                new_turn_id(),
                true,
                old_wall,
            )
            .await
            .unwrap();
        ledger_cap
            .mark_delivered(&p_cap_done, old_wall)
            .await
            .unwrap();

        let p_cap_active = InboundId::from_platform_id("tg", "603");
        let m_cap_active = InboundId::from_platform_id("tg", "604");
        ledger_cap
            .register_turn(
                p_cap_active.clone(),
                vec![m_cap_active.clone()],
                chat.clone(),
                sid,
                new_turn_id(),
                false,
                old_wall,
            )
            .await
            .unwrap();
        ledger_cap.mark_accepted(&p_cap_active).await.unwrap();

        // 再写入新相册组触发逼近上限写前清理
        let p_cap_fresh = InboundId::from_platform_id("tg", "605");
        let m_cap_fresh = InboundId::from_platform_id("tg", "606");
        ledger_cap
            .register_turn(
                p_cap_fresh.clone(),
                vec![m_cap_fresh.clone()],
                chat,
                sid,
                new_turn_id(),
                false,
                Utc::now(),
            )
            .await
            .expect("逼近上限写前清理过期 DeliveredTurn 与关联 Merged 后应写入成功");

        assert_eq!(
            ledger_cap.inbound_record(&p_cap_done).await,
            None,
            "逼近上限写前清理应删除过期的首条 DeliveredTurn"
        );
        assert_eq!(
            ledger_cap.inbound_record(&m_cap_done).await,
            None,
            "逼近上限写前清理应删除超过 24h+5m 且关联 turn 已转 DeliveredTurn 的 Merged"
        );
        assert!(
            matches!(
                ledger_cap.inbound_record(&m_cap_active).await,
                Some(InboundRecord::Merged { .. })
            ),
            "逼近上限写前清理必须保留关联 turn 仍处于 Accepted 的过期 Merged"
        );
        assert!(
            matches!(
                ledger_cap.inbound_record(&m_cap_fresh).await,
                Some(InboundRecord::Merged { .. })
            ),
            "新写入的相册 Merged 条目应成功提交"
        );
    }
}
