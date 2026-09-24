// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{GitSessionGraphStore, PendingTurnTip, helpers, pending, session, state};
use crate::pending::{PendingStage, TurnStartedFacts};
use crate::{GraphHead, GraphSession, Result, SessionError, TurnCommit, TurnId};

/// 固定引用的观察结果，不承诺刚读到的引用已经通过发布者的持久屏障。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionReadView {
    pub state_oid: String,
    pub session: GraphSession,
    pub head: GraphHead,
    pub head_commit_id: Option<String>,
    pub active_turn: Option<PendingTurnTip>,
    pub requested_turn: Option<StoredTurn>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StoredTurn {
    Pending(PendingTurnTip),
    Committed {
        commit: TurnCommit,
        started: TurnStartedFacts,
    },
}

impl StoredTurn {
    pub fn started(&self) -> &TurnStartedFacts {
        match self {
            Self::Pending(tip) => match &tip.snapshot.stage {
                PendingStage::TurnStarted(facts) => facts,
                _ => tip.snapshot.root.as_ref().expect("validated pending root"),
            },
            Self::Committed { started, .. } => started,
        }
    }
}

/// 固定 TurnCommit/journal 的 transcript 字节页；游标始终落在 UTF-8 边界。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptPage {
    pub jsonl: String,
    pub next_offset: Option<usize>,
}

/// 不含配置或正文的公开状态；正文经固定 TurnCommit/journal OID 分页投影。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionObservation {
    pub state_oid: String,
    pub session: GraphSession,
    pub head: GraphHead,
    pub head_commit_id: Option<String>,
    pub branches: Vec<crate::GraphRef>,
    pub commit_count: usize,
    pub active_turn: Option<PendingObservation>,
    pub requested_turn: Option<TurnObservation>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PendingObservation {
    pub turn_id: TurnId,
    pub journal_oid: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TurnObservation {
    Pending(PendingObservation),
    Committed(Box<CommittedTurnObservation>),
}

/// parents 是对话父边而非 journal 父边，读者可按固定 OID 遍历所有 merge 侧。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommittedTurnObservation {
    pub commit_id: String,
    pub turn_id: TurnId,
    pub parent_ids: Vec<String>,
    pub agent_name: String,
    pub model: String,
    pub provider: String,
    pub outcome: crate::TurnOutcome,
    pub origin: crate::ClosureOrigin,
    pub usage: Option<crate::TurnUsage>,
    pub workspace_history_checkpoint: crate::WorkspaceHistoryCheckpoint,
    pub created_at: String,
}

impl GitSessionGraphStore {
    /// 固定对话 tip 的历史页；保留既有 Git 遍历顺序与 merge 去重语义。
    pub fn history_page(
        &self,
        session_id: &str,
        head: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<String>> {
        if !(1..=256).contains(&limit) {
            return Err(SessionError::InvalidInput(
                "history page limit must be 1..=256".into(),
            ));
        }
        self.observe_commit(session_id, head)?;
        // 当前内核 rev_walk 会物化 O(历史节点) 身份；正文仍逐 Turn 分页读取。
        let ids =
            helpers::walk_commits_oldest_first(self.kernel(), helpers::parse_oid(head)?, None)?;
        let mut turns = Vec::new();
        let mut skipped = 0;
        for id in ids {
            if pending::is_journal_commit(self.kernel(), id)? {
                continue;
            }
            if skipped < offset {
                skipped += 1;
                continue;
            }
            self.observe_commit(session_id, &id.to_string())?;
            turns.push(id.to_string());
            if turns.len() == limit {
                break;
            }
        }
        Ok(turns)
    }

    pub fn observe(&self, session_id: &str, turn_id: Option<TurnId>) -> Result<SessionObservation> {
        let (state_oid, state, head_oid, active_oid, pin_oid, branches) = {
            let _read = self.kernel().write_lock()?;
            let (oid, state) = state::read_state(self.kernel(), session_id)?;
            let head = session::resolve_head_oid(self.kernel(), &state, session_id)?;
            let active = self
                .kernel()
                .ref_target(&pending::active_turn_ref_name(session_id)?)?;
            let pin = turn_id
                .map(|id| -> Result<_> {
                    Ok(self
                        .kernel()
                        .ref_target(&pending::turn_pin_ref_name(session_id, &id.to_string())?)?)
                })
                .transpose()?
                .flatten();
            let branches = session::enumerate_session_branches(self.kernel(), session_id)?;
            (oid, state, head, active, pin, branches)
        };
        let active_turn = active_oid
            .map(|oid| -> Result<_> {
                #[derive(serde::Deserialize)]
                struct Identity {
                    schema_version: u32,
                    session_id: String,
                    turn_id: TurnId,
                }
                let bytes = self.kernel().read_tree_blob(oid, "pending.json")?;
                let identity: Identity = serde_json::from_slice(&bytes).map_err(|_| {
                    SessionError::JournalCorruption {
                        detail: "invalid pending observation identity".into(),
                    }
                })?;
                if identity.session_id != session_id
                    || identity.schema_version != crate::pending::PENDING_SNAPSHOT_SCHEMA_VERSION
                {
                    return Err(SessionError::JournalCorruption {
                        detail: "pending observation identity differs".into(),
                    });
                }
                Ok(PendingObservation {
                    turn_id: identity.turn_id,
                    journal_oid: oid.to_string(),
                })
            })
            .transpose()?;
        let requested_turn = match (turn_id, pin_oid) {
            (Some(id), Some(oid)) => {
                let turn = self.observe_commit(session_id, &oid.to_string())?;
                if turn.turn_id != id
                    || active_turn
                        .as_ref()
                        .is_some_and(|active| active.turn_id == id)
                {
                    return Err(SessionError::JournalCorruption {
                        detail: "Turn pin observation identity differs".into(),
                    });
                }
                Some(TurnObservation::Committed(Box::new(turn)))
            }
            (Some(id), None) => active_turn
                .as_ref()
                .filter(|active| active.turn_id == id)
                .cloned()
                .map(TurnObservation::Pending),
            _ => None,
        };
        Ok(SessionObservation {
            state_oid: state_oid.to_string(),
            session: state.to_graph_session(),
            head: state.graph_head(),
            head_commit_id: head_oid.map(|oid| oid.to_string()),
            branches,
            commit_count: session::count_turn_commits(self.kernel(), head_oid)?,
            active_turn,
            requested_turn,
        })
    }

    pub fn observe_commit(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> Result<CommittedTurnObservation> {
        let oid = helpers::parse_oid(commit_id)?;
        let manifest = helpers::parse_manifest(&self.kernel().read_tree_blob_bounded(
            oid,
            "turn/manifest.json",
            65_536,
        )?)?;
        if manifest.session_id != session_id {
            return Err(SessionError::NotFound(commit_id.into()));
        }
        Ok(CommittedTurnObservation {
            commit_id: commit_id.into(),
            turn_id: manifest.turn_id,
            parent_ids: manifest.parent_oids,
            agent_name: manifest.agent_name,
            model: manifest.model,
            provider: manifest.provider,
            outcome: manifest.outcome,
            origin: manifest.origin,
            usage: manifest
                .usage
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| SessionError::JournalCorruption {
                    detail: "invalid usage in Turn manifest".into(),
                })?,
            workspace_history_checkpoint: manifest.workspace_history_checkpoint,
            created_at: manifest.timestamp,
        })
    }
}

impl GitSessionGraphStore {
    /// 只在捕获 state/head/active/pin 时持仓库锁，后续解析不可变对象；不取全 Turn 锁。
    pub fn read_view(&self, session_id: &str, turn_id: Option<TurnId>) -> Result<SessionReadView> {
        let (state_oid, state, head_oid, active_oid, pin_oid) = {
            let _read = self.kernel().write_lock()?;
            let (state_oid, state) = state::read_state(self.kernel(), session_id)?;
            let head_oid = session::resolve_head_oid(self.kernel(), &state, session_id)?;
            let active_oid = self
                .kernel()
                .ref_target(&pending::active_turn_ref_name(session_id)?)?;
            let pin_oid = match turn_id {
                Some(id) => self
                    .kernel()
                    .ref_target(&pending::turn_pin_ref_name(session_id, &id.to_string())?)?,
                None => None,
            };
            (state_oid, state, head_oid, active_oid, pin_oid)
        };
        let active_turn = active_oid
            .map(|oid| self.read_pending_snapshot_at(oid))
            .transpose()?;
        if let Some(tip) = &active_turn {
            let facts =
                match &tip.snapshot.stage {
                    PendingStage::TurnStarted(facts) => facts,
                    _ => tip.snapshot.root.as_ref().ok_or_else(|| {
                        SessionError::JournalCorruption {
                            detail: "pending root missing".into(),
                        }
                    })?,
                };
            if facts.session_id.as_str() != session_id
                || facts.project_store_key.to_string() != state.project_store_key
            {
                return Err(SessionError::JournalCorruption {
                    detail: "pending identity differs from fixed Session state".into(),
                });
            }
        }
        let requested_turn = match (turn_id, pin_oid) {
            (Some(id), Some(oid)) => {
                if active_turn
                    .as_ref()
                    .is_some_and(|tip| tip.snapshot.turn_id == id)
                {
                    return Err(SessionError::JournalCorruption {
                        detail: "same Turn is active and committed".into(),
                    });
                }
                let commit = self.read_finalized_turn(session_id, &id, &oid.to_string())?;
                let parents = self.kernel().commit_parents(oid)?;
                let journal = parents
                    .first()
                    .ok_or_else(|| SessionError::JournalCorruption {
                        detail: "pinned Turn has no committed journal".into(),
                    })?;
                let tip = self.read_pending_snapshot_at(*journal)?;
                if tip.snapshot.session_id.as_str() != session_id
                    || tip.snapshot.turn_id != id
                    || !matches!(tip.snapshot.stage, PendingStage::Committed(_))
                {
                    return Err(SessionError::JournalCorruption {
                        detail: "pinned Turn journal identity differs".into(),
                    });
                }
                let root = self.resolve_turn_started(&tip)?;
                let PendingStage::TurnStarted(started) = root.snapshot.stage else {
                    unreachable!()
                };
                if started.project_store_key.to_string() != state.project_store_key {
                    return Err(SessionError::JournalCorruption {
                        detail: "pinned Turn project differs".into(),
                    });
                }
                Some(StoredTurn::Committed { commit, started })
            }
            (Some(id), None) => active_turn
                .as_ref()
                .filter(|tip| tip.snapshot.turn_id == id)
                .cloned()
                .map(StoredTurn::Pending),
            (None, _) => None,
        };
        Ok(SessionReadView {
            state_oid: state_oid.to_string(),
            session: state.to_graph_session(),
            head: state.graph_head(),
            head_commit_id: head_oid.map(|oid| oid.to_string()),
            active_turn,
            requested_turn,
        })
    }

    /// 固定 TurnCommit 或 pending journal 的正文页；只投影用户和累积消息，不输出配置。
    pub fn read_transcript_page(
        &self,
        session_id: &str,
        commit_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<TranscriptPage> {
        if !(1..=65_536).contains(&limit) {
            return Err(SessionError::InvalidInput(
                "transcript page limit must be 1..=65536 bytes".into(),
            ));
        }
        let oid = helpers::parse_oid(commit_id)?;
        // 单页与 IPC 仍有界；底层复用合法 Turn 的整 blob 读取，内存为 O(单 Turn blob)。
        let bytes = if pending::is_journal_commit(self.kernel(), oid)? {
            let tip = self.read_pending_snapshot_at(oid)?;
            if tip.snapshot.session_id.as_str() != session_id {
                return Err(SessionError::NotFound(commit_id.into()));
            }
            helpers::messages_to_jsonl(&tip.snapshot.turn_messages()?)?.into_bytes()
        } else {
            let manifest =
                helpers::parse_manifest(&self.kernel().read_tree_blob(oid, "turn/manifest.json")?)?;
            if manifest.session_id != session_id {
                return Err(SessionError::NotFound(commit_id.into()));
            }
            self.kernel().read_tree_blob(oid, "turn/messages.jsonl")?
        };
        let jsonl = std::str::from_utf8(&bytes)
            .map_err(|err| SessionError::Git(format!("transcript UTF-8: {err}")))?;
        if !jsonl.is_char_boundary(offset) {
            return Err(SessionError::InvalidInput(
                "transcript offset is not a UTF-8 boundary".into(),
            ));
        }
        let mut end = offset.saturating_add(limit).min(bytes.len());
        while !jsonl.is_char_boundary(end) {
            end -= 1;
        }
        if end == offset && end < bytes.len() {
            return Err(SessionError::InvalidInput(
                "page limit cannot hold the next UTF-8 character".into(),
            ));
        }
        Ok(TranscriptPage {
            jsonl: jsonl[offset..end].to_owned(),
            next_offset: (end < bytes.len()).then_some(end),
        })
    }
}
