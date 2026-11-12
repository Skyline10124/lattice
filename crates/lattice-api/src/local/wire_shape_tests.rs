// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Local IPC v63 线上形状基线与协议版本门禁。

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use lattice_conversation::{
    BoundToolCall, CostEstimate, CostSummary, Effort, IndeterminateCause, InvocationId,
    MessagePart, MessageRole, MessageSemantic, ModelId, ModelMessageAddress, ModelRef,
    NativeMessage, OpaqueProviderState, Protocol, ProviderId, ReplayOrigin, ResultPayload,
    SessionId, StoredToolResult, SuccessContent, TokenSummary, TokenUsage, ToolCallAddress,
    ToolCallValidation, ToolError, ToolResultOutcome, TurnId, UsageCompleteness, UsageSummary,
    arguments_digest, canonical_json, tool_content_digest,
};
use lattice_llm::{
    Availability, ConfigurationRequirement, ModelCandidate, ModelCandidateQuery, ModelCandidates,
    ModelPrice, ModelWarning,
    auth::{AuthStatus, ProviderAuthStatus, RevocationStatus},
};
use lattice_session::{
    AgentName, ProjectPath, RetryPolicy, TurnTimeout,
    store::{
        AttemptDetail, AttemptDiagnostic, AttemptEnd, AttemptEndDetail, AttemptProgress,
        AttemptProgressDetail, AttemptRecord, BindingIdentity, ConversationEntry, EffectiveProfile,
        FrozenTurnConfig, InvocationBinding, InvocationSlot, LogRow, MaterialSummary, Node,
        NodeHeader, NodeId, NodeKind, Notice, NoticeLevel, Page, PublicTurnOutcome, RetryNotice,
        SessionListRow as StoreSessionListRow, SessionLog, SessionRecord, SessionSettings,
        SessionUsage, SettingsVersion, SlotDetail, SlotProgress, SlotReason, SlotState, StepDetail,
        StepEnd, StepKey, StepProgress, StopCause, ToolCatalogEntry, ToolCatalogSnapshot,
        TurnDetail, TurnExecutionLimits, TurnLogSummary, TurnSummary, TurnUsageRow,
    },
};
use serde::{Deserialize, Serialize};

use super::{
    Acceptance, AuthCatalogOutcome, AuthLoginOutcome, AuthRemoval, ConfigureReply,
    ConfigureRequest, ConfigureSession, ConflictReason, DaemonStatus, EffortAdjustment, LocalError,
    LocalErrorKind, LocalHistory, RecoveryCause, RecoveryNeed, SessionHead, SessionListRow,
    StoreDiagnostic, TextSegment, TextUpdate, TurnLookup, TurnRef, TurnRequest, TurnView,
    ViewUpdate, auth::AuthKey, auth::login::LoginAction, auth::login::LoginResponse,
    ipc::PROTOCOL_VERSION, types::AuthLoggedOut, types::Request, types::Response,
};

const SNAPSHOT_RELATIVE_PATH: &str = "src/local/wire_shape.json";
const UPDATE_ENV_VAR: &str = "LATTICE_UPDATE_WIRE_SHAPE";
const MAX_REPORTED_DIFFS: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireShapeSnapshot {
    protocol_version: u8,
    cases: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum GateDecision {
    Pass,
    Write(WireShapeSnapshot),
    Fail(String),
}

/// 纯判定函数：根据快照文件内容、当前协议版本与样例、以及是否请求更新决定门禁结果。
fn decide_snapshot(
    stored: Option<&WireShapeSnapshot>,
    current_version: u8,
    current_cases: &BTreeMap<String, String>,
    update_requested: bool,
) -> GateDecision {
    let Some(stored) = stored else {
        if update_requested {
            return GateDecision::Write(WireShapeSnapshot {
                protocol_version: current_version,
                cases: current_cases.clone(),
            });
        }
        return GateDecision::Fail(format!(
            "线上形状快照文件不存在；如需生成基线，请设置 {UPDATE_ENV_VAR}=1 重新运行测试"
        ));
    };

    if current_version < stored.protocol_version {
        return GateDecision::Fail(format!(
            "当前 PROTOCOL_VERSION（{current_version}）小于快照文件记录的版本（{}），不允许回退协议版本",
            stored.protocol_version
        ));
    }

    if current_version > stored.protocol_version {
        if update_requested {
            return GateDecision::Write(WireShapeSnapshot {
                protocol_version: current_version,
                cases: current_cases.clone(),
            });
        }
        let diffs = mismatched_case_names(&stored.cases, current_cases);
        let diff_hint = format_diff_hint(&diffs);
        return GateDecision::Fail(format!(
            "当前 PROTOCOL_VERSION（{current_version}）大于快照文件版本（{}）；请以 {UPDATE_ENV_VAR}=1 请求更新重新生成快照{diff_hint}",
            stored.protocol_version
        ));
    }

    if &stored.cases == current_cases {
        return GateDecision::Pass;
    }

    let diffs = mismatched_case_names(&stored.cases, current_cases);
    let diff_hint = format_diff_hint(&diffs);
    GateDecision::Fail(format!(
        "线上形状变了（PROTOCOL_VERSION = {current_version}），请先升 PROTOCOL_VERSION，再以 {UPDATE_ENV_VAR}=1 请求更新重新生成{diff_hint}"
    ))
}

fn mismatched_case_names(
    stored: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Vec<String> {
    let all_names: BTreeSet<&str> = stored
        .keys()
        .chain(current.keys())
        .map(String::as_str)
        .collect();
    let mut diffs = Vec::new();
    for name in all_names {
        if stored.get(name) != current.get(name) {
            diffs.push(name.to_owned());
        }
    }
    diffs
}

fn format_diff_hint(diffs: &[String]) -> String {
    if diffs.is_empty() {
        return String::new();
    }
    let preview: Vec<&str> = diffs
        .iter()
        .take(MAX_REPORTED_DIFFS)
        .map(String::as_str)
        .collect();
    format!(
        "（共 {} 条样例不一致，前 {} 条：{}）",
        diffs.len(),
        preview.len(),
        preview.join(", ")
    )
}

enum RootFrame {
    Request(Request),
    Response(Box<Response>),
    LoginAction(LoginAction),
    LoginResponse(LoginResponse),
}

struct WireCase {
    name: &'static str,
    frame: RootFrame,
}

impl WireCase {
    fn encode_and_round_trip(&self) -> String {
        match &self.frame {
            RootFrame::Request(value) => round_trip_json(self.name, value),
            RootFrame::Response(value) => round_trip_json(self.name, value.as_ref()),
            RootFrame::LoginAction(value) => round_trip_json(self.name, value),
            RootFrame::LoginResponse(value) => round_trip_json(self.name, value),
        }
    }
}

fn round_trip_json<T>(case_name: &str, value: &T) -> String
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let encoded = serde_json::to_string(value)
        .unwrap_or_else(|err| panic!("样例 {case_name} 序列化失败：{err}"));
    let decoded: T = serde_json::from_str(&encoded)
        .unwrap_or_else(|err| panic!("样例 {case_name} 反序列化失败：{err}\nJSON: {encoded}"));
    let reencoded = serde_json::to_string(&decoded)
        .unwrap_or_else(|err| panic!("样例 {case_name} 二次序列化失败：{err}"));
    assert_eq!(
        reencoded, encoded,
        "样例 {case_name} 往返序列化结果与原始字符串不一致"
    );
    encoded
}

// ── 穷尽守卫（第 2 条范围内的全部 enum：lattice-api 与经 IPC 上线的 lattice-llm 类型）──

/// 同一份声明同时生成穷尽守卫与该类型的变体全集：新增变体时 `match` 编译失败，
/// 补上分支后全集随之包含它，覆盖断言随即要求补上样例。
macro_rules! variant_guard {
    ($guard:ident, $all:ident, $ty:ty { $($pattern:pat => $name:literal,)+ }) => {
        const $all: &[&str] = &[$($name),+];
        fn $guard(value: &$ty) -> &'static str {
            match value {
                $($pattern => $name,)+
            }
        }
    };
}

// 对应类型：Request（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_request, REQUEST_VARIANTS, Request {
    Request::Status => "Status",
    Request::Stop => "Stop",
    Request::AuthSet { .. } => "AuthSet",
    Request::AuthRemove { .. } => "AuthRemove",
    Request::AuthLogin { .. } => "AuthLogin",
    Request::AuthLogout { .. } => "AuthLogout",
    Request::AuthStatus {} => "AuthStatus",
    Request::Models { .. } => "Models",
    Request::ModelsRefresh {} => "ModelsRefresh",
    Request::ResolveSession { .. } => "ResolveSession",
    Request::List { .. } => "List",
    Request::Log { .. } => "Log",
    Request::Usage { .. } => "Usage",
    Request::Turn { .. } => "Turn",
    Request::Head { .. } => "Head",
    Request::History { .. } => "History",
    Request::Rewind { .. } => "Rewind",
    Request::Configure { .. } => "Configure",
    Request::Submit { .. } => "Submit",
    Request::Cancel { .. } => "Cancel",
    Request::Watch { .. } => "Watch",
});

// 对应类型：Response（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_response, RESPONSE_VARIANTS, Response {
    Response::Status(_) => "Status",
    Response::AuthUpdated => "AuthUpdated",
    Response::AuthRemoved(_) => "AuthRemoved",
    Response::AuthLoggedOut(_) => "AuthLoggedOut",
    Response::AuthStatus(_) => "AuthStatus",
    Response::Models(_) => "Models",
    Response::ModelsRefreshed => "ModelsRefreshed",
    Response::Session(_) => "Session",
    Response::Sessions(_) => "Sessions",
    Response::Log(_) => "Log",
    Response::Usage(_) => "Usage",
    Response::Turn(_) => "Turn",
    Response::Head(_) => "Head",
    Response::History(_) => "History",
    Response::Rewind(_) => "Rewind",
    Response::Configure(_) => "Configure",
    Response::Acceptance(_) => "Acceptance",
    Response::View(_) => "View",
    Response::Cancelled { .. } => "Cancelled",
    Response::Error(_) => "Error",
});

// 对应类型：LoginAction（crates/lattice-api/src/local/auth/login.rs）
variant_guard!(guard_login_action, LOGIN_ACTION_VARIANTS, LoginAction {
    LoginAction::Callback { .. } => "Callback",
    LoginAction::Cancel {} => "Cancel",
});

// 对应类型：LoginResponse（crates/lattice-api/src/local/auth/login.rs）
variant_guard!(guard_login_response, LOGIN_RESPONSE_VARIANTS, LoginResponse {
    LoginResponse::AuthLoginAuthorization { .. } => "AuthLoginAuthorization",
    LoginResponse::AuthLoginResult(_) => "AuthLoginResult",
    LoginResponse::Error(_) => "Error",
});

// 对应类型：ConfigureSession（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_configure_session, CONFIGURE_SESSION_VARIANTS, ConfigureSession {
    ConfigureSession::New(_) => "New",
    ConfigureSession::Existing(_) => "Existing",
});

// 对应类型：TurnLookup（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_turn_lookup, TURN_LOOKUP_VARIANTS, TurnLookup {
    TurnLookup::Accepted { .. } => "Accepted",
    TurnLookup::InFlight => "InFlight",
});

// 对应类型：Acceptance（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_acceptance, ACCEPTANCE_VARIANTS, Acceptance {
    Acceptance::Accepted { .. } => "Accepted",
    Acceptance::ConfirmationUnknown { .. } => "ConfirmationUnknown",
});

// 对应类型：ViewUpdate（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_view_update, VIEW_UPDATE_VARIANTS, ViewUpdate {
    ViewUpdate::Head(_) => "Head",
    ViewUpdate::Entry { .. } => "Entry",
    ViewUpdate::Text(_) => "Text",
});

// 对应类型：TextSegment（crates/lattice-api/src/turn/types.rs）
variant_guard!(guard_text_segment, TEXT_SEGMENT_VARIANTS, TextSegment {
    TextSegment::Text { .. } => "Text",
    TextSegment::Reasoning { .. } => "Reasoning",
});

// 对应类型：LocalErrorKind（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_local_error_kind, LOCAL_ERROR_KIND_VARIANTS, LocalErrorKind {
    LocalErrorKind::BadRequest => "BadRequest",
    LocalErrorKind::NotFound => "NotFound",
    LocalErrorKind::Conflict => "Conflict",
    LocalErrorKind::Busy => "Busy",
    LocalErrorKind::Stopping => "Stopping",
    LocalErrorKind::StoreUnavailable => "StoreUnavailable",
    LocalErrorKind::Internal => "Internal",
    LocalErrorKind::NotRunning => "NotRunning",
    LocalErrorKind::Transport => "Transport",
    LocalErrorKind::Protocol => "Protocol",
    LocalErrorKind::Permission => "Permission",
    LocalErrorKind::AlreadyRunning => "AlreadyRunning",
    LocalErrorKind::IncompatibleBackend => "IncompatibleBackend",
});

// 对应类型：ConflictReason / RequestConflict（crates/lattice-api/src/session.rs）
variant_guard!(guard_conflict_reason, CONFLICT_REASON_VARIANTS, ConflictReason {
    ConflictReason::TurnIdUsed => "TurnIdUsed",
    ConflictReason::SessionExists => "SessionExists",
    ConflictReason::BindingMismatch => "BindingMismatch",
    ConflictReason::SettingsChanged => "SettingsChanged",
});

// 对应类型：RecoveryCause（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_recovery_cause, RECOVERY_CAUSE_VARIANTS, RecoveryCause {
    RecoveryCause::CommitFailed => "CommitFailed",
    RecoveryCause::WriteDisabled => "WriteDisabled",
    RecoveryCause::WorkerFailed => "WorkerFailed",
    RecoveryCause::ReopenFailed => "ReopenFailed",
});

// 对应类型：StoreDiagnostic（crates/lattice-api/src/local/types.rs）
variant_guard!(guard_store_diagnostic, STORE_DIAGNOSTIC_VARIANTS, StoreDiagnostic {
    StoreDiagnostic::NotInitialized { .. } => "NotInitialized",
    StoreDiagnostic::LegacyStores { .. } => "LegacyStores",
    StoreDiagnostic::UnsupportedVersion { .. } => "UnsupportedVersion",
    StoreDiagnostic::Corruption { .. } => "Corruption",
});

// 对应类型：AuthLoginOutcome（crates/lattice-api/src/local/auth/login.rs）
variant_guard!(guard_auth_login_outcome, AUTH_LOGIN_OUTCOME_VARIANTS, AuthLoginOutcome {
    AuthLoginOutcome::SignedIn { .. } => "SignedIn",
    AuthLoginOutcome::Cancelled {} => "Cancelled",
});

// 对应类型：AuthCatalogOutcome（crates/lattice-api/src/local/auth/login.rs）
variant_guard!(guard_auth_catalog_outcome, AUTH_CATALOG_OUTCOME_VARIANTS, AuthCatalogOutcome {
    AuthCatalogOutcome::Refreshed { .. } => "Refreshed",
    AuthCatalogOutcome::Failed { .. } => "Failed",
});

// 对应类型：ModelCandidateQuery（crates/lattice-llm/src/models.rs）
variant_guard!(guard_model_candidate_query, MODEL_CANDIDATE_QUERY_VARIANTS, ModelCandidateQuery {
    ModelCandidateQuery::List { .. } => "List",
    ModelCandidateQuery::Target(_) => "Target",
});

// 对应类型：Availability（crates/lattice-llm/src/availability.rs）
variant_guard!(guard_availability, AVAILABILITY_VARIANTS, Availability {
    Availability::Ready => "Ready",
    Availability::Unconfigured { .. } => "Unconfigured",
    Availability::Invalid { .. } => "Invalid",
});

// 对应类型：ConfigurationRequirement（crates/lattice-llm/src/availability.rs）
variant_guard!(guard_configuration_requirement, CONFIGURATION_REQUIREMENT_VARIANTS, ConfigurationRequirement {
    ConfigurationRequirement::Declaration => "Declaration",
    ConfigurationRequirement::ApiKey => "ApiKey",
    ConfigurationRequirement::ChatgptLogin => "ChatgptLogin",
});

// 对应类型：RevocationStatus（crates/lattice-llm/src/auth/siwc/mod.rs）
variant_guard!(guard_revocation_status, REVOCATION_STATUS_VARIANTS, RevocationStatus {
    RevocationStatus::Confirmed => "Confirmed",
    RevocationStatus::Unconfirmed => "Unconfirmed",
    RevocationStatus::NotNeeded => "NotNeeded",
});

// ── 覆盖追踪器：从已构造的真实样例收集每个变体、Option 分支与集合空/非空 ──

#[derive(Default)]
struct CoverageTracker {
    enums: BTreeMap<&'static str, BTreeSet<&'static str>>,
    options: BTreeMap<&'static str, (bool, bool)>,
    collections: BTreeMap<&'static str, (bool, bool)>,
}

impl CoverageTracker {
    fn record_variant(&mut self, enum_name: &'static str, variant: &'static str) {
        self.enums.entry(enum_name).or_default().insert(variant);
    }

    fn record_option<T>(&mut self, field: &'static str, value: &Option<T>) {
        let entry = self.options.entry(field).or_insert((false, false));
        if value.is_some() {
            entry.0 = true;
        } else {
            entry.1 = true;
        }
    }

    fn record_slice<T>(&mut self, field: &'static str, items: &[T]) {
        let entry = self.collections.entry(field).or_insert((false, false));
        if items.is_empty() {
            entry.0 = true;
        } else {
            entry.1 = true;
        }
    }

    fn observe_case(&mut self, case: &WireCase) {
        match &case.frame {
            RootFrame::Request(req) => self.observe_request(req),
            RootFrame::Response(resp) => self.observe_response(resp),
            RootFrame::LoginAction(action) => self.observe_login_action(action),
            RootFrame::LoginResponse(resp) => self.observe_login_response(resp),
        }
    }

    fn observe_request(&mut self, req: &Request) {
        self.record_variant("Request", guard_request(req));
        match req {
            Request::Status
            | Request::Stop
            | Request::AuthSet {
                provider: _,
                key: AuthKey(_),
            }
            | Request::AuthRemove { provider: _ }
            | Request::AuthLogin { provider: _ }
            | Request::AuthLogout { provider: _ }
            | Request::AuthStatus {}
            | Request::ModelsRefresh {}
            | Request::ResolveSession { prefix: _ }
            | Request::Log { session_id: _ }
            | Request::Usage { session_id: _ }
            | Request::Turn { turn_id: _ }
            | Request::Head { session_id: _ }
            | Request::History {
                session_id: _,
                before: _,
                turns: _,
            }
            | Request::Rewind {
                session_id: _,
                target: _,
                workspace: _,
            }
            | Request::Cancel { turn_id: _ }
            | Request::Watch { session_id: _ } => {}
            Request::Models { query } => self.observe_model_candidate_query(query),
            Request::List { after, limit: _ } => {
                self.record_option("Request::List.after", after);
            }
            Request::Configure { request } => self.observe_configure_request(request),
            Request::Submit { request } => self.observe_turn_request(request),
        }
    }

    fn observe_configure_request(&mut self, req: &ConfigureRequest) {
        let ConfigureRequest {
            session,
            workspace: _,
            agent,
            model,
            effort,
        } = req;
        self.record_variant("ConfigureSession", guard_configure_session(session));
        self.record_option("ConfigureRequest.agent", agent);
        self.record_option("ConfigureRequest.model", model);
        self.record_option("ConfigureRequest.effort", effort);
    }

    fn observe_turn_request(&mut self, req: &TurnRequest) {
        let TurnRequest {
            turn_id: _,
            session: _,
            workspace: _,
            input: _,
            expected_settings,
        } = req;
        self.record_option("TurnRequest.expected_settings", expected_settings);
    }

    fn observe_model_candidate_query(&mut self, query: &ModelCandidateQuery) {
        self.record_variant("ModelCandidateQuery", guard_model_candidate_query(query));
        match query {
            ModelCandidateQuery::List {
                provider,
                ready_only: _,
            } => {
                self.record_option("ModelCandidateQuery::List.provider", provider);
            }
            ModelCandidateQuery::Target(_) => {}
        }
    }

    fn observe_login_action(&mut self, action: &LoginAction) {
        self.record_variant("LoginAction", guard_login_action(action));
        match action {
            LoginAction::Callback { url: AuthKey(_) } | LoginAction::Cancel {} => {}
        }
    }

    fn observe_login_response(&mut self, resp: &LoginResponse) {
        self.record_variant("LoginResponse", guard_login_response(resp));
        match resp {
            LoginResponse::AuthLoginAuthorization {
                authorization_url: AuthKey(_),
            } => {}
            LoginResponse::AuthLoginResult(outcome) => self.observe_auth_login_outcome(outcome),
            LoginResponse::Error(error) => self.observe_local_error(error),
        }
    }

    fn observe_auth_login_outcome(&mut self, outcome: &AuthLoginOutcome) {
        self.record_variant("AuthLoginOutcome", guard_auth_login_outcome(outcome));
        match outcome {
            AuthLoginOutcome::SignedIn { status, catalog } => {
                self.observe_provider_auth_status(status);
                self.record_variant("AuthCatalogOutcome", guard_auth_catalog_outcome(catalog));
                match catalog {
                    AuthCatalogOutcome::Refreshed { model_count: _ }
                    | AuthCatalogOutcome::Failed { warning: _ } => {}
                }
            }
            AuthLoginOutcome::Cancelled {} => {}
        }
    }

    fn observe_response(&mut self, resp: &Response) {
        self.record_variant("Response", guard_response(resp));
        match resp {
            Response::Status(DaemonStatus {
                epoch: _,
                stopping: _,
                owners: _,
                unconfirmed_operations: _,
            })
            | Response::AuthUpdated
            | Response::ModelsRefreshed
            | Response::Session(_)
            | Response::Log(_)
            | Response::Usage(_)
            | Response::Rewind(_)
            | Response::Cancelled { owned: _ } => {}
            Response::AuthRemoved(AuthRemoval {
                removed: _,
                revocation,
            }) => {
                self.record_option("AuthRemoval.revocation", revocation);
                if let Some(status) = revocation {
                    self.record_variant("RevocationStatus", guard_revocation_status(status));
                }
            }
            Response::AuthLoggedOut(AuthLoggedOut { revocation }) => {
                self.record_variant("RevocationStatus", guard_revocation_status(revocation));
            }
            Response::AuthStatus(AuthStatus {
                providers_path: _,
                providers,
                error,
            }) => {
                self.record_slice("AuthStatus.providers", providers);
                self.record_option("AuthStatus.error", error);
                for provider in providers {
                    self.observe_provider_auth_status(provider);
                }
            }
            Response::Models(ModelCandidates { models, warnings }) => {
                self.record_slice("ModelCandidates.models", models);
                self.record_slice("ModelCandidates.warnings", warnings);
                for candidate in models {
                    self.observe_model_candidate(candidate);
                }
                for warning in warnings {
                    let ModelWarning {
                        provider,
                        message: _,
                    } = warning;
                    self.record_option("ModelWarning.provider", provider);
                }
            }
            Response::Sessions(Page { items, next }) => {
                self.record_slice("Response::Sessions.items", items);
                self.record_option("Response::Sessions.next", next);
                for row in items {
                    let SessionListRow {
                        session: _,
                        owner: _,
                    } = row;
                }
            }
            Response::Turn(lookup) => {
                self.record_variant("TurnLookup", guard_turn_lookup(lookup));
                match lookup {
                    TurnLookup::Accepted { turn: _ } | TurnLookup::InFlight => {}
                }
            }
            Response::Head(head) => self.observe_session_head(head),
            Response::History(LocalHistory { entries }) => {
                self.record_slice("LocalHistory.entries", entries);
            }
            Response::Configure(ConfigureReply { head, adjustment }) => {
                self.observe_session_head(head);
                self.record_option("ConfigureReply.adjustment", adjustment);
                if let Some(EffortAdjustment { from, to }) = adjustment {
                    self.record_option("EffortAdjustment.from", from);
                    self.record_option("EffortAdjustment.to", to);
                }
            }
            Response::Acceptance(acceptance) => {
                self.record_variant("Acceptance", guard_acceptance(acceptance));
                match acceptance {
                    Acceptance::Accepted {
                        turn:
                            TurnView {
                                session_id: _,
                                turn_id: _,
                                user_node: _,
                                agent_name: _,
                                accepted_at: _,
                                ended_at: _,
                                summary: _,
                                usage: _,
                            },
                    }
                    | Acceptance::ConfirmationUnknown {
                        turn_id: _,
                        session_id: _,
                    } => {}
                }
            }
            Response::View(view) => {
                self.record_variant("ViewUpdate", guard_view_update(view));
                match view {
                    ViewUpdate::Head(head) => self.observe_session_head(head),
                    ViewUpdate::Entry { entry: _ } => {}
                    ViewUpdate::Text(TextUpdate {
                        node: _,
                        attempt: _,
                        offset: _,
                        segments,
                    }) => {
                        self.record_slice("TextUpdate.segments", segments);
                        for segment in segments {
                            self.record_variant("TextSegment", guard_text_segment(segment));
                        }
                    }
                }
            }
            Response::Error(error) => self.observe_local_error(error),
        }
    }

    fn observe_provider_auth_status(&mut self, status: &ProviderAuthStatus) {
        let ProviderAuthStatus {
            provider: _,
            availability,
            email,
            expires_at,
        } = status;
        self.observe_availability(availability);
        self.record_option("ProviderAuthStatus.email", email);
        self.record_option("ProviderAuthStatus.expires_at", expires_at);
    }

    fn observe_model_candidate(&mut self, candidate: &ModelCandidate) {
        let ModelCandidate {
            provider: _,
            model: _,
            name,
            context,
            output,
            supported_efforts,
            cost,
            availability,
        } = candidate;
        self.record_option("ModelCandidate.name", name);
        self.record_option("ModelCandidate.context", context);
        self.record_option("ModelCandidate.output", output);
        self.record_slice("ModelCandidate.supported_efforts", supported_efforts);
        self.record_option("ModelCandidate.cost", cost);
        if let Some(ModelPrice {
            input,
            output,
            cache_read,
            cache_write,
        }) = cost
        {
            self.record_option("ModelPrice.input", input);
            self.record_option("ModelPrice.output", output);
            self.record_option("ModelPrice.cache_read", cache_read);
            self.record_option("ModelPrice.cache_write", cache_write);
        }
        self.observe_availability(availability);
    }

    fn observe_availability(&mut self, availability: &Availability) {
        self.record_variant("Availability", guard_availability(availability));
        match availability {
            Availability::Ready | Availability::Invalid { reason: _ } => {}
            Availability::Unconfigured { requirement } => {
                self.record_variant(
                    "ConfigurationRequirement",
                    guard_configuration_requirement(requirement),
                );
            }
        }
    }

    fn observe_session_head(&mut self, head: &SessionHead) {
        let SessionHead {
            session_id: _,
            position: _,
            agent: _,
            model,
            provider,
            effort,
            settings_version: _,
            turn_id,
            owner: _,
            diagnostic,
            path_replaced: _,
        } = head;
        self.record_option("SessionHead.model", model);
        self.record_option("SessionHead.provider", provider);
        self.record_option("SessionHead.effort", effort);
        self.record_option("SessionHead.turn_id", turn_id);
        self.record_option("SessionHead.diagnostic", diagnostic);
        if let Some(error) = diagnostic {
            self.observe_local_error(error);
        }
    }

    fn observe_local_error(&mut self, error: &LocalError) {
        let LocalError {
            kind,
            message: _,
            conflict,
            recovery,
            store,
        } = error;
        self.record_variant("LocalErrorKind", guard_local_error_kind(kind));
        self.record_option("LocalError.conflict", conflict);
        if let Some(reason) = conflict {
            self.record_variant("ConflictReason", guard_conflict_reason(reason));
        }
        self.record_option("LocalError.recovery", recovery);
        if let Some(RecoveryNeed { turn, cause }) = recovery {
            self.record_option("RecoveryNeed.turn", turn);
            if let Some(TurnRef {
                session_id: _,
                turn_id: _,
            }) = turn
            {}
            self.record_variant("RecoveryCause", guard_recovery_cause(cause));
        }
        self.record_option("LocalError.store", store);
        if let Some(diag) = store {
            self.record_variant("StoreDiagnostic", guard_store_diagnostic(diag));
            match diag.as_ref() {
                StoreDiagnostic::NotInitialized { path: _ }
                | StoreDiagnostic::UnsupportedVersion {
                    path: _,
                    table: _,
                    key: _,
                    found: _,
                    supported: _,
                }
                | StoreDiagnostic::Corruption { table: _, key: _ } => {}
                StoreDiagnostic::LegacyStores { paths } => {
                    self.record_slice("StoreDiagnostic::LegacyStores.paths", paths);
                }
            }
        }
    }
}

// ── 确定性夹具与构造辅助 ──

fn session_id_primary() -> SessionId {
    SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").expect("解析固定主 SessionId")
}

fn session_id_secondary() -> SessionId {
    SessionId::parse("7a6b5c4d3e2f4a1b9c8d7e6f5a4b3c2d").expect("解析固定次 SessionId")
}

fn turn_id_primary() -> TurnId {
    TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("解析固定主 TurnId")
}

fn turn_id_secondary() -> TurnId {
    TurnId::parse("7a6b5c4d-3e2f-4a1b-9c8d-7e6f5a4b3c2d").expect("解析固定次 TurnId")
}

fn invocation_id_primary() -> InvocationId {
    InvocationId::parse("11111111-1111-4111-8111-111111111111").expect("解析固定主 InvocationId")
}

fn invocation_id_secondary() -> InvocationId {
    InvocationId::parse("22222222-2222-4222-8222-222222222222").expect("解析固定次 InvocationId")
}

fn timestamp_accepted() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-05-01T10:20:30Z")
        .expect("解析固定开始时间")
        .with_timezone(&Utc)
}

fn timestamp_ended() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-05-01T10:20:35.123456Z")
        .expect("解析固定结束时间")
        .with_timezone(&Utc)
}

fn non_utf8_workspace() -> PathBuf {
    PathBuf::from(OsStr::from_bytes(b"/workspace/proj-\xff\xfe/repo"))
}

fn utf8_workspace() -> PathBuf {
    PathBuf::from("/workspace/lattice")
}

fn non_utf8_project_path() -> ProjectPath {
    ProjectPath::from_bytes(b"/workspace/proj-\xff\xfe/repo").expect("构造非 UTF-8 ProjectPath")
}

fn agent_coder() -> AgentName {
    AgentName::parse("coder").expect("解析固定 AgentName")
}

fn provider_openai() -> ProviderId {
    ProviderId::parse("openai").expect("解析固定 ProviderId")
}

fn model_gpt5() -> ModelId {
    ModelId::parse("gpt-5").expect("解析固定 ModelId")
}

fn model_ref_nested() -> ModelRef {
    ModelRef::parse("local/org/model-v1").expect("解析带斜杠模型名的 ModelRef")
}

fn fixed_settings_version() -> SettingsVersion {
    let settings = SessionSettings {
        profile: EffectiveProfile {
            agent_name: agent_coder(),
            description: Some("代码审查助手".to_owned()),
            default_model: Some(ModelRef::new(provider_openai(), model_gpt5())),
            default_effort: Some(Effort::Medium),
            timeout_secs: TurnTimeout::new(120).expect("构造固定 TurnTimeout"),
            retry: RetryPolicy::new(2, 500, 3_000).expect("构造固定 RetryPolicy"),
        },
        system_prompt: Some("遵循仓库规范".to_owned()),
        model: Some(ModelRef::new(provider_openai(), model_gpt5())),
        effort: Some(Effort::Medium),
    };
    settings.version().expect("计算固定 SettingsVersion")
}

fn fixed_config_digest() -> [u8; 32] {
    let catalog = ToolCatalogSnapshot::new(vec![ToolCatalogEntry {
        name: "read_file".to_owned(),
        tool_identity: "lancet:read_file@v1".to_owned(),
        description: "读取工作区文件".to_owned(),
        input_schema: serde_json::json!({"type": "object"}),
    }])
    .expect("构造固定工具目录快照");
    let config = FrozenTurnConfig {
        agent_name: agent_coder(),
        model: model_gpt5(),
        provider: provider_openai(),
        effort: Some(Effort::High),
        system_prompt: Some("遵循仓库规范".to_owned()),
        catalog,
        limits: fixed_limits(),
    };
    config.digest().expect("计算固定配置摘要")
}

fn fixed_limits() -> TurnExecutionLimits {
    TurnExecutionLimits {
        timeout_secs: TurnTimeout::new(120).expect("构造固定 TurnTimeout"),
        max_model_steps: 32,
        max_model_attempts: 3,
    }
}

fn sample_session_record() -> SessionRecord {
    SessionRecord {
        session_id: session_id_primary(),
        project_path: non_utf8_project_path(),
        current: NodeId(2),
        created_at: timestamp_accepted(),
        updated_at: timestamp_ended(),
    }
}

fn sample_usage_summary() -> UsageSummary {
    UsageSummary {
        attempts: 2,
        partial_attempts: 1,
        kind: UsageCompleteness::Partial,
        input: TokenSummary {
            known: Some(1_200),
            complete: true,
            overflowed: false,
        },
        cache_read: TokenSummary {
            known: Some(400),
            complete: true,
            overflowed: false,
        },
        cache_write: TokenSummary {
            known: Some(100),
            complete: false,
            overflowed: false,
        },
        output: TokenSummary {
            known: Some(350),
            complete: true,
            overflowed: false,
        },
        reasoning: TokenSummary {
            known: Some(120),
            complete: true,
            overflowed: false,
        },
        cost: CostSummary {
            known: Some(CostEstimate {
                usd: 0.0125,
                exact: false,
            }),
            unknown_attempts: 1,
            overflowed: false,
        },
    }
}

fn sample_turn_summary_completed() -> TurnSummary {
    TurnSummary {
        outcome: PublicTurnOutcome::Completed,
        notice: Some(Notice {
            level: NoticeLevel::Warn,
            text: "第 1 次模型请求超时，已自动重试".to_owned(),
        }),
        progress: StepProgress::Closed {
            end: StepEnd::Finish,
        },
    }
}

fn sample_binding_identity(part_ordinal: u32, arguments: &serde_json::Value) -> BindingIdentity {
    BindingIdentity {
        session_id: session_id_primary(),
        turn_id: turn_id_primary(),
        model_step_ordinal: 0,
        part_ordinal,
        method: "read_file".to_owned(),
        tool_identity: "lancet:read_file@v1".to_owned(),
        arguments_digest: arguments_digest(arguments),
    }
}

fn sample_step_detail() -> StepDetail {
    let args_1 = serde_json::json!({"path": "Cargo.toml"});
    let args_2 = serde_json::json!({"path": "missing.txt"});
    let result_value = serde_json::json!({"lines": 50});
    let canonical_result = canonical_json(&result_value);
    let result_digest = tool_content_digest(&canonical_result);
    let result_len = canonical_result.len() as u64;

    StepDetail {
        node: NodeId(2),
        key: StepKey {
            turn_id: turn_id_primary(),
            ordinal: 0,
        },
        progress: StepProgress::Closed {
            end: StepEnd::Finish,
        },
        end: StepEnd::Finish,
        attempts: vec![
            AttemptDetail {
                attempt: 0,
                origin: ReplayOrigin {
                    protocol: Protocol::Responses,
                    provider_id: provider_openai(),
                },
                usage: None,
                usage_kind: UsageCompleteness::Partial,
                cost: None,
                progress: AttemptProgressDetail::Ended {
                    end: AttemptEnd::Stopped {
                        cause: StopCause::ProviderError { retryable: true },
                    },
                    diagnostic: Some("upstream 503".to_owned()),
                },
                end: AttemptEndDetail::Stopped {
                    cause: StopCause::ProviderError { retryable: true },
                    diagnostic: Some("upstream 503".to_owned()),
                },
                retry_delay_ms: Some(500),
            },
            AttemptDetail {
                attempt: 1,
                origin: ReplayOrigin {
                    protocol: Protocol::Responses,
                    provider_id: provider_openai(),
                },
                usage: Some(TokenUsage {
                    input: Some(1_200),
                    cache_read: Some(400),
                    cache_write: Some(100),
                    output: Some(350),
                    reasoning: Some(120),
                }),
                usage_kind: UsageCompleteness::Complete,
                cost: Some(CostEstimate {
                    usd: 0.0125,
                    exact: true,
                }),
                progress: AttemptProgressDetail::Ended {
                    end: AttemptEnd::Completed,
                    diagnostic: None,
                },
                end: AttemptEndDetail::Completed {
                    step: ModelMessageAddress {
                        session_id: session_id_primary(),
                        turn_id: turn_id_primary(),
                        model_step_ordinal: 0,
                    },
                },
                retry_delay_ms: None,
            },
        ],
        slots: vec![
            SlotDetail {
                invocation_id: invocation_id_primary(),
                binding: sample_binding_identity(2, &args_1),
                progress: SlotState::Terminal {
                    outcome: ToolResultOutcome::Succeeded,
                    reason: None,
                    material: MaterialSummary::Available {
                        digest: result_digest,
                        byte_length: result_len,
                    },
                },
                outcome: ToolResultOutcome::Succeeded,
                reason: None,
                material: MaterialSummary::Available {
                    digest: result_digest,
                    byte_length: result_len,
                },
            },
            SlotDetail {
                invocation_id: invocation_id_secondary(),
                binding: sample_binding_identity(3, &args_2),
                progress: SlotState::Terminal {
                    outcome: ToolResultOutcome::Indeterminate,
                    reason: Some(SlotReason::Indeterminate(IndeterminateCause::NoResult)),
                    material: MaterialSummary::NoContent,
                },
                outcome: ToolResultOutcome::Indeterminate,
                reason: Some(SlotReason::Indeterminate(IndeterminateCause::NoResult)),
                material: MaterialSummary::NoContent,
            },
        ],
        updated_at: timestamp_ended(),
    }
}

fn sample_turn_detail() -> TurnDetail {
    TurnDetail {
        turn_id: turn_id_primary(),
        session_id: session_id_primary(),
        user_node: NodeId(1),
        agent_name: agent_coder(),
        accepted_at: timestamp_accepted(),
        ended_at: timestamp_ended(),
        summary: sample_turn_summary_completed(),
        usage: Box::new(sample_usage_summary()),
        steps: vec![sample_step_detail()],
    }
}

fn sample_turn_view() -> TurnView {
    TurnView {
        session_id: session_id_primary(),
        turn_id: turn_id_primary(),
        user_node: NodeId(1),
        agent_name: "coder".to_owned(),
        accepted_at: timestamp_accepted(),
        ended_at: timestamp_ended(),
        summary: sample_turn_summary_completed(),
        usage: Box::new(sample_usage_summary()),
    }
}

fn sample_user_entry() -> ConversationEntry {
    ConversationEntry::UserMessage {
        node: NodeId(1),
        content: Node::UserMessage {
            parent: NodeId(0),
            turn_id: turn_id_primary(),
            accepted_at: timestamp_accepted(),
            agent_name: agent_coder(),
            model: model_gpt5(),
            provider: provider_openai(),
            effort: Some(Effort::High),
            limits: fixed_limits(),
            config_digest: fixed_config_digest(),
            message: NativeMessage {
                role: MessageRole::User,
                provider_origin: None,
                parts: vec![MessagePart {
                    semantic: MessageSemantic::Text {
                        text: "读取 Cargo.toml".to_owned(),
                    },
                    replay_state: None,
                }],
            },
        },
        ordinal: 1,
    }
}

fn sample_step_entry() -> ConversationEntry {
    let args_1 = serde_json::json!({"path": "Cargo.toml"});
    let args_2 = serde_json::json!({"path": "missing.txt"});
    let result_value = serde_json::json!({"lines": 50});
    let canonical_result = canonical_json(&result_value);
    let result_digest = tool_content_digest(&canonical_result);
    let result_len = canonical_result.len() as u64;

    let address_1 = ToolCallAddress {
        message: ModelMessageAddress {
            session_id: session_id_primary(),
            turn_id: turn_id_primary(),
            model_step_ordinal: 0,
        },
        part_ordinal: 2,
    };
    let address_2 = ToolCallAddress {
        message: ModelMessageAddress {
            session_id: session_id_primary(),
            turn_id: turn_id_primary(),
            model_step_ordinal: 0,
        },
        part_ordinal: 3,
    };

    ConversationEntry::Step {
        node: NodeId(2),
        content: Node::Step {
            parent: NodeId(1),
            turn_id: turn_id_primary(),
            step_ordinal: 0,
            progress: StepProgress::Closed {
                end: StepEnd::Finish,
            },
            attempts: vec![
                AttemptRecord {
                    origin: ReplayOrigin {
                        protocol: Protocol::Responses,
                        provider_id: provider_openai(),
                    },
                    usage: None,
                    usage_kind: UsageCompleteness::Partial,
                    cost: None,
                    progress: AttemptProgress::Ended {
                        end: AttemptEnd::Stopped {
                            cause: StopCause::InvalidBatch,
                        },
                        diagnostic: AttemptDiagnostic {
                            provider_error: Some("invalid tool arguments".to_owned()),
                            invalid_batch: Some(ToolCallValidation::MalformedArguments {
                                call_ordinal: 0,
                            }),
                        },
                    },
                    retry_delay_ms: Some(500),
                },
                AttemptRecord {
                    origin: ReplayOrigin {
                        protocol: Protocol::Responses,
                        provider_id: provider_openai(),
                    },
                    usage: Some(TokenUsage {
                        input: Some(1_200),
                        cache_read: Some(400),
                        cache_write: Some(100),
                        output: Some(350),
                        reasoning: Some(120),
                    }),
                    usage_kind: UsageCompleteness::Complete,
                    cost: Some(CostEstimate {
                        usd: 0.0125,
                        exact: true,
                    }),
                    progress: AttemptProgress::Ended {
                        end: AttemptEnd::Completed,
                        diagnostic: AttemptDiagnostic {
                            provider_error: None,
                            invalid_batch: None,
                        },
                    },
                    retry_delay_ms: None,
                },
            ],
            reply: Some(NativeMessage {
                role: MessageRole::Assistant,
                provider_origin: Some(ReplayOrigin {
                    protocol: Protocol::Responses,
                    provider_id: provider_openai(),
                }),
                parts: vec![
                    MessagePart {
                        semantic: MessageSemantic::Reasoning {
                            text: "需要先查看工作区配置".to_owned(),
                        },
                        replay_state: Some(
                            OpaqueProviderState::new("rs_opaque_sig_01")
                                .expect("构造固定 OpaqueProviderState"),
                        ),
                    },
                    MessagePart {
                        semantic: MessageSemantic::Text {
                            text: "正在读取文件".to_owned(),
                        },
                        replay_state: None,
                    },
                    MessagePart {
                        semantic: MessageSemantic::ToolCall {
                            call: BoundToolCall {
                                invocation_id: invocation_id_primary(),
                                provider_call_id: Some("call_01".to_owned()),
                                name: "read_file".to_owned(),
                                arguments_digest: arguments_digest(&args_1),
                                arguments: args_1.clone(),
                                address: address_1,
                            },
                        },
                        replay_state: None,
                    },
                    MessagePart {
                        semantic: MessageSemantic::ToolCall {
                            call: BoundToolCall {
                                invocation_id: invocation_id_secondary(),
                                provider_call_id: None,
                                name: "read_file".to_owned(),
                                arguments_digest: arguments_digest(&args_2),
                                arguments: args_2.clone(),
                                address: address_2,
                            },
                        },
                        replay_state: None,
                    },
                ],
            }),
            slots: vec![
                InvocationSlot {
                    binding: InvocationBinding {
                        invocation_id: invocation_id_primary(),
                        identity: sample_binding_identity(2, &args_1),
                    },
                    progress: SlotProgress::Terminal {
                        result: StoredToolResult {
                            invocation_id: invocation_id_primary(),
                            name: "read_file".to_owned(),
                            payload: ResultPayload::Succeeded(SuccessContent::Json {
                                value: result_value,
                                digest: result_digest,
                                byte_length: result_len,
                            }),
                        },
                    },
                },
                InvocationSlot {
                    binding: InvocationBinding {
                        invocation_id: invocation_id_secondary(),
                        identity: sample_binding_identity(3, &args_2),
                    },
                    progress: SlotProgress::Terminal {
                        result: StoredToolResult {
                            invocation_id: invocation_id_secondary(),
                            name: "read_file".to_owned(),
                            payload: ResultPayload::Failed(ToolError {
                                code: "not_found".to_owned(),
                                message: "missing.txt 不存在".to_owned(),
                            }),
                        },
                    },
                },
            ],
            updated_at: timestamp_ended(),
        },
        step: sample_step_detail(),
        retries: vec![RetryNotice {
            attempt: 1,
            delay_ms: 500,
            notice: Notice {
                level: NoticeLevel::Warn,
                text: "第 1 次模型请求失败，500ms 后重试".to_owned(),
            },
        }],
        summary: Some(sample_turn_summary_completed()),
    }
}

fn sample_session_head_idle() -> SessionHead {
    SessionHead {
        session_id: session_id_primary(),
        position: NodeId(0),
        agent: "coder".to_owned(),
        model: None,
        provider: None,
        effort: None,
        settings_version: fixed_settings_version(),
        turn_id: None,
        owner: false,
        diagnostic: None,
        path_replaced: false,
    }
}

fn sample_session_head_active() -> SessionHead {
    SessionHead {
        session_id: session_id_primary(),
        position: NodeId(2),
        agent: "coder".to_owned(),
        model: Some(model_gpt5()),
        provider: Some(provider_openai()),
        effort: Some(Effort::High),
        settings_version: fixed_settings_version(),
        turn_id: Some(turn_id_primary()),
        owner: true,
        diagnostic: Some(plain_error(
            LocalErrorKind::StoreUnavailable,
            "session store unavailable",
        )),
        path_replaced: true,
    }
}

fn plain_error(kind: LocalErrorKind, message: &str) -> LocalError {
    LocalError {
        kind,
        message: message.to_owned(),
        conflict: None,
        recovery: None,
        store: None,
    }
}

fn build_wire_cases() -> Vec<WireCase> {
    vec![
        // ── Request（21 个变体）──
        WireCase {
            name: "request/status",
            frame: RootFrame::Request(Request::Status),
        },
        WireCase {
            name: "request/stop",
            frame: RootFrame::Request(Request::Stop),
        },
        WireCase {
            name: "request/auth_set",
            frame: RootFrame::Request(Request::AuthSet {
                provider: "openai".to_owned(),
                key: AuthKey("sk-wire-shape-fixed-secret".to_owned()),
            }),
        },
        WireCase {
            name: "request/auth_remove",
            frame: RootFrame::Request(Request::AuthRemove {
                provider: "openai".to_owned(),
            }),
        },
        WireCase {
            name: "request/auth_login",
            frame: RootFrame::Request(Request::AuthLogin {
                provider: "chatgpt".to_owned(),
            }),
        },
        WireCase {
            name: "request/auth_logout",
            frame: RootFrame::Request(Request::AuthLogout {
                provider: "chatgpt".to_owned(),
            }),
        },
        WireCase {
            name: "request/auth_status",
            frame: RootFrame::Request(Request::AuthStatus {}),
        },
        WireCase {
            name: "request/models/list_all",
            frame: RootFrame::Request(Request::Models {
                query: ModelCandidateQuery::List {
                    provider: None,
                    ready_only: false,
                },
            }),
        },
        WireCase {
            name: "request/models/list_provider_ready",
            frame: RootFrame::Request(Request::Models {
                query: ModelCandidateQuery::List {
                    provider: Some(provider_openai()),
                    ready_only: true,
                },
            }),
        },
        WireCase {
            name: "request/models/target",
            frame: RootFrame::Request(Request::Models {
                query: ModelCandidateQuery::Target(model_ref_nested()),
            }),
        },
        WireCase {
            name: "request/models_refresh",
            frame: RootFrame::Request(Request::ModelsRefresh {}),
        },
        WireCase {
            name: "request/resolve_session",
            frame: RootFrame::Request(Request::ResolveSession {
                prefix: "3f8b2c1a".to_owned(),
            }),
        },
        WireCase {
            name: "request/list/first_page",
            frame: RootFrame::Request(Request::List {
                after: None,
                limit: 50,
            }),
        },
        WireCase {
            name: "request/list/next_page",
            frame: RootFrame::Request(Request::List {
                after: Some(session_id_primary()),
                limit: 25,
            }),
        },
        WireCase {
            name: "request/log",
            frame: RootFrame::Request(Request::Log {
                session_id: session_id_primary(),
            }),
        },
        WireCase {
            name: "request/usage",
            frame: RootFrame::Request(Request::Usage {
                session_id: "3f8b2c1a".to_owned(),
            }),
        },
        WireCase {
            name: "request/turn",
            frame: RootFrame::Request(Request::Turn {
                turn_id: turn_id_primary(),
            }),
        },
        WireCase {
            name: "request/head",
            frame: RootFrame::Request(Request::Head {
                session_id: session_id_primary(),
            }),
        },
        WireCase {
            name: "request/history",
            frame: RootFrame::Request(Request::History {
                session_id: session_id_primary(),
                before: NodeId(5),
                turns: 10,
            }),
        },
        WireCase {
            name: "request/rewind",
            frame: RootFrame::Request(Request::Rewind {
                session_id: session_id_primary(),
                target: NodeId(1),
                workspace: non_utf8_workspace(),
            }),
        },
        WireCase {
            name: "request/configure/new_with_agent",
            frame: RootFrame::Request(Request::Configure {
                request: ConfigureRequest {
                    session: ConfigureSession::New(session_id_primary()),
                    workspace: non_utf8_workspace(),
                    agent: Some(agent_coder()),
                    model: None,
                    effort: None,
                },
            }),
        },
        WireCase {
            name: "request/configure/existing_with_model_and_effort",
            frame: RootFrame::Request(Request::Configure {
                request: ConfigureRequest {
                    session: ConfigureSession::Existing(session_id_primary()),
                    workspace: utf8_workspace(),
                    agent: None,
                    model: Some(model_ref_nested()),
                    effort: Some(Effort::High),
                },
            }),
        },
        WireCase {
            name: "request/submit/with_expected_settings",
            frame: RootFrame::Request(Request::Submit {
                request: TurnRequest {
                    turn_id: turn_id_primary(),
                    session: session_id_primary(),
                    workspace: non_utf8_workspace(),
                    input: "请检查工作区结构".to_owned(),
                    expected_settings: Some(fixed_settings_version()),
                },
            }),
        },
        WireCase {
            name: "request/submit/without_expected_settings",
            frame: RootFrame::Request(Request::Submit {
                request: TurnRequest {
                    turn_id: turn_id_secondary(),
                    session: session_id_primary(),
                    workspace: utf8_workspace(),
                    input: "继续".to_owned(),
                    expected_settings: None,
                },
            }),
        },
        WireCase {
            name: "request/cancel",
            frame: RootFrame::Request(Request::Cancel {
                turn_id: turn_id_primary(),
            }),
        },
        WireCase {
            name: "request/watch",
            frame: RootFrame::Request(Request::Watch {
                session_id: session_id_primary(),
            }),
        },
        // ── LoginAction（2 个变体）──
        WireCase {
            name: "login_action/callback",
            frame: RootFrame::LoginAction(LoginAction::Callback {
                url: AuthKey(
                    "http://127.0.0.1:1455/auth/callback?code=fixed-code&state=fixed-state"
                        .to_owned(),
                ),
            }),
        },
        WireCase {
            name: "login_action/cancel",
            frame: RootFrame::LoginAction(LoginAction::Cancel {}),
        },
        // ── LoginResponse（3 个变体）──
        WireCase {
            name: "login_response/authorization",
            frame: RootFrame::LoginResponse(LoginResponse::AuthLoginAuthorization {
                authorization_url: AuthKey(
                    "https://auth.openai.com/oauth/authorize?client_id=test&state=fixed".to_owned(),
                ),
            }),
        },
        WireCase {
            name: "login_response/result/signed_in_catalog_refreshed",
            frame: RootFrame::LoginResponse(LoginResponse::AuthLoginResult(
                AuthLoginOutcome::SignedIn {
                    status: ProviderAuthStatus {
                        provider: "chatgpt".to_owned(),
                        availability: Availability::Ready,
                        email: Some("dev@example.com".to_owned()),
                        expires_at: Some("2026-05-02T00:00:00Z".to_owned()),
                    },
                    catalog: AuthCatalogOutcome::Refreshed { model_count: 4 },
                },
            )),
        },
        WireCase {
            name: "login_response/result/signed_in_catalog_failed",
            frame: RootFrame::LoginResponse(LoginResponse::AuthLoginResult(
                AuthLoginOutcome::SignedIn {
                    status: ProviderAuthStatus {
                        provider: "chatgpt".to_owned(),
                        availability: Availability::Ready,
                        email: None,
                        expires_at: None,
                    },
                    catalog: AuthCatalogOutcome::Failed {
                        warning: "account model directory refresh timed out".to_owned(),
                    },
                },
            )),
        },
        WireCase {
            name: "login_response/result/cancelled",
            frame: RootFrame::LoginResponse(LoginResponse::AuthLoginResult(
                AuthLoginOutcome::Cancelled {},
            )),
        },
        WireCase {
            name: "login_response/error",
            frame: RootFrame::LoginResponse(LoginResponse::Error(plain_error(
                LocalErrorKind::Transport,
                "token endpoint unreachable",
            ))),
        },
        // ── Response（20 个变体）──
        WireCase {
            name: "response/status",
            frame: RootFrame::Response(Box::new(Response::Status(DaemonStatus {
                epoch: "epoch-fixed-01".to_owned(),
                stopping: true,
                owners: 2,
                unconfirmed_operations: 1,
            }))),
        },
        WireCase {
            name: "response/auth_updated",
            frame: RootFrame::Response(Box::new(Response::AuthUpdated)),
        },
        WireCase {
            name: "response/auth_removed/without_revocation",
            frame: RootFrame::Response(Box::new(Response::AuthRemoved(AuthRemoval {
                removed: true,
                revocation: None,
            }))),
        },
        WireCase {
            name: "response/auth_removed/with_revocation_confirmed",
            frame: RootFrame::Response(Box::new(Response::AuthRemoved(AuthRemoval {
                removed: true,
                revocation: Some(RevocationStatus::Confirmed),
            }))),
        },
        WireCase {
            name: "response/auth_logged_out/unconfirmed",
            frame: RootFrame::Response(Box::new(Response::AuthLoggedOut(AuthLoggedOut {
                revocation: RevocationStatus::Unconfirmed,
            }))),
        },
        WireCase {
            name: "response/auth_logged_out/not_needed",
            frame: RootFrame::Response(Box::new(Response::AuthLoggedOut(AuthLoggedOut {
                revocation: RevocationStatus::NotNeeded,
            }))),
        },
        WireCase {
            name: "response/auth_status/empty_without_error",
            frame: RootFrame::Response(Box::new(Response::AuthStatus(AuthStatus {
                providers_path: PathBuf::from("/home/dev/.lattice/providers.toml"),
                providers: Vec::new(),
                error: None,
            }))),
        },
        WireCase {
            name: "response/auth_status/with_providers_and_error",
            frame: RootFrame::Response(Box::new(Response::AuthStatus(AuthStatus {
                providers_path: PathBuf::from("/home/dev/.lattice/providers.toml"),
                providers: vec![
                    ProviderAuthStatus {
                        provider: "local".to_owned(),
                        availability: Availability::Unconfigured {
                            requirement: ConfigurationRequirement::Declaration,
                        },
                        email: None,
                        expires_at: None,
                    },
                    ProviderAuthStatus {
                        provider: "openai".to_owned(),
                        availability: Availability::Unconfigured {
                            requirement: ConfigurationRequirement::ApiKey,
                        },
                        email: None,
                        expires_at: None,
                    },
                    ProviderAuthStatus {
                        provider: "chatgpt".to_owned(),
                        availability: Availability::Unconfigured {
                            requirement: ConfigurationRequirement::ChatgptLogin,
                        },
                        email: Some("dev@example.com".to_owned()),
                        expires_at: Some("2026-05-02T00:00:00Z".to_owned()),
                    },
                    ProviderAuthStatus {
                        provider: "anthropic".to_owned(),
                        availability: Availability::Invalid {
                            reason: "auth.json: permission denied".to_owned(),
                        },
                        email: None,
                        expires_at: None,
                    },
                    ProviderAuthStatus {
                        provider: "ollama".to_owned(),
                        availability: Availability::Ready,
                        email: None,
                        expires_at: None,
                    },
                ],
                error: Some("auth.json: invalid credential structure".to_owned()),
            }))),
        },
        WireCase {
            name: "response/models/empty",
            frame: RootFrame::Response(Box::new(Response::Models(ModelCandidates {
                models: Vec::new(),
                warnings: Vec::new(),
            }))),
        },
        WireCase {
            name: "response/models/with_warnings",
            frame: RootFrame::Response(Box::new(Response::Models(ModelCandidates {
                models: vec![
                    ModelCandidate {
                        provider: provider_openai(),
                        model: model_gpt5(),
                        name: Some("GPT-5".to_owned()),
                        context: Some(400_000),
                        output: Some(128_000),
                        supported_efforts: vec![Effort::Low, Effort::Medium, Effort::High],
                        cost: Some(ModelPrice {
                            input: Some(1.25),
                            output: Some(10.0),
                            cache_read: Some(0.125),
                            cache_write: Some(1.5),
                        }),
                        availability: Availability::Ready,
                    },
                    ModelCandidate {
                        provider: ProviderId::parse("local").expect("解析 local ProviderId"),
                        model: ModelId::parse("org/model-v1").expect("解析嵌套 ModelId"),
                        name: None,
                        context: None,
                        output: None,
                        supported_efforts: Vec::new(),
                        cost: None,
                        availability: Availability::Unconfigured {
                            requirement: ConfigurationRequirement::ApiKey,
                        },
                    },
                    ModelCandidate {
                        provider: ProviderId::parse("local").expect("解析 local ProviderId"),
                        model: ModelId::parse("broken").expect("解析 broken ModelId"),
                        name: None,
                        context: Some(32_768),
                        output: None,
                        supported_efforts: vec![Effort::Off],
                        cost: Some(ModelPrice {
                            input: None,
                            output: None,
                            cache_read: None,
                            cache_write: None,
                        }),
                        availability: Availability::Invalid {
                            reason: "unsupported effort".to_owned(),
                        },
                    },
                ],
                warnings: vec![
                    ModelWarning {
                        provider: Some("openai".to_owned()),
                        message:
                            "catalog \"openai\" has no metadata; only handwritten models are available"
                                .to_owned(),
                    },
                    ModelWarning {
                        provider: None,
                        message: "metadata cache is stale".to_owned(),
                    },
                ],
            }))),
        },
        WireCase {
            name: "response/models_refreshed",
            frame: RootFrame::Response(Box::new(Response::ModelsRefreshed)),
        },
        WireCase {
            name: "response/session",
            frame: RootFrame::Response(Box::new(Response::Session(sample_session_record()))),
        },
        WireCase {
            name: "response/sessions/empty",
            frame: RootFrame::Response(Box::new(Response::Sessions(Page {
                items: Vec::new(),
                next: None,
            }))),
        },
        WireCase {
            name: "response/sessions/page_with_next",
            frame: RootFrame::Response(Box::new(Response::Sessions(Page {
                items: vec![SessionListRow {
                    session: StoreSessionListRow {
                        session_id: session_id_primary(),
                        project_path: non_utf8_project_path(),
                        current: NodeId(2),
                        node_count: 3,
                        unclosed_turn: Some(turn_id_primary()),
                        updated_at: timestamp_ended(),
                    },
                    owner: true,
                }],
                next: Some(session_id_secondary()),
            }))),
        },
        WireCase {
            name: "response/log",
            frame: RootFrame::Response(Box::new(Response::Log(SessionLog {
                session: sample_session_record(),
                current_agent: Some(agent_coder()),
                unclosed_turn: Some(turn_id_secondary()),
                nodes: vec![
                    LogRow {
                        node: NodeHeader {
                            id: NodeId(0),
                            parent: None,
                            turn_id: None,
                            kind: NodeKind::Root,
                        },
                        current: false,
                        turn_start: None,
                    },
                    LogRow {
                        node: NodeHeader {
                            id: NodeId(1),
                            parent: Some(NodeId(0)),
                            turn_id: Some(turn_id_primary()),
                            kind: NodeKind::UserMessage,
                        },
                        current: false,
                        turn_start: Some(TurnLogSummary {
                            turn_id: turn_id_primary(),
                            summary: sample_turn_summary_completed(),
                            agent_name: agent_coder(),
                        }),
                    },
                    LogRow {
                        node: NodeHeader {
                            id: NodeId(2),
                            parent: Some(NodeId(1)),
                            turn_id: Some(turn_id_primary()),
                            kind: NodeKind::Step { ordinal: 0 },
                        },
                        current: true,
                        turn_start: None,
                    },
                ],
            }))),
        },
        WireCase {
            name: "response/usage",
            frame: RootFrame::Response(Box::new(Response::Usage(Box::new(SessionUsage {
                session_id: session_id_primary(),
                position: NodeId(2),
                turns: vec![TurnUsageRow {
                    turn_id: turn_id_primary(),
                    ordinal: 1,
                    provider: provider_openai(),
                    model: model_gpt5(),
                    usage: sample_usage_summary(),
                }],
                path_total: sample_usage_summary(),
                session_total: sample_usage_summary(),
            })))),
        },
        WireCase {
            name: "response/turn/accepted",
            frame: RootFrame::Response(Box::new(Response::Turn(TurnLookup::Accepted {
                turn: sample_turn_detail(),
            }))),
        },
        WireCase {
            name: "response/turn/in_flight",
            frame: RootFrame::Response(Box::new(Response::Turn(TurnLookup::InFlight))),
        },
        WireCase {
            name: "response/head/idle_minimal",
            frame: RootFrame::Response(Box::new(Response::Head(sample_session_head_idle()))),
        },
        WireCase {
            name: "response/head/active_with_diagnostic",
            frame: RootFrame::Response(Box::new(Response::Head(sample_session_head_active()))),
        },
        WireCase {
            name: "response/history/empty",
            frame: RootFrame::Response(Box::new(Response::History(LocalHistory {
                entries: Vec::new(),
            }))),
        },
        WireCase {
            name: "response/history/with_entries",
            frame: RootFrame::Response(Box::new(Response::History(LocalHistory {
                entries: vec![sample_user_entry(), sample_step_entry()],
            }))),
        },
        WireCase {
            name: "response/rewind",
            frame: RootFrame::Response(Box::new(Response::Rewind(sample_session_record()))),
        },
        WireCase {
            name: "response/configure/without_adjustment",
            frame: RootFrame::Response(Box::new(Response::Configure(ConfigureReply {
                head: sample_session_head_idle(),
                adjustment: None,
            }))),
        },
        WireCase {
            name: "response/configure/with_adjustment_some_to_none",
            frame: RootFrame::Response(Box::new(Response::Configure(ConfigureReply {
                head: sample_session_head_active(),
                adjustment: Some(EffortAdjustment {
                    from: Some(Effort::High),
                    to: None,
                }),
            }))),
        },
        WireCase {
            name: "response/configure/with_adjustment_none_to_some",
            frame: RootFrame::Response(Box::new(Response::Configure(ConfigureReply {
                head: sample_session_head_active(),
                adjustment: Some(EffortAdjustment {
                    from: None,
                    to: Some(Effort::Medium),
                }),
            }))),
        },
        WireCase {
            name: "response/acceptance/accepted",
            frame: RootFrame::Response(Box::new(Response::Acceptance(Acceptance::Accepted {
                turn: sample_turn_view(),
            }))),
        },
        WireCase {
            name: "response/acceptance/confirmation_unknown",
            frame: RootFrame::Response(Box::new(Response::Acceptance(
                Acceptance::ConfirmationUnknown {
                    turn_id: turn_id_primary(),
                    session_id: session_id_primary(),
                },
            ))),
        },
        WireCase {
            name: "response/view/head",
            frame: RootFrame::Response(Box::new(Response::View(ViewUpdate::Head(
                sample_session_head_active(),
            )))),
        },
        WireCase {
            name: "response/view/entry",
            frame: RootFrame::Response(Box::new(Response::View(ViewUpdate::Entry {
                entry: Box::new(sample_user_entry()),
            }))),
        },
        WireCase {
            name: "response/view/text_empty",
            frame: RootFrame::Response(Box::new(Response::View(ViewUpdate::Text(TextUpdate {
                node: NodeId(2),
                attempt: 0,
                offset: 0,
                segments: Vec::new(),
            })))),
        },
        WireCase {
            name: "response/view/text_with_segments",
            frame: RootFrame::Response(Box::new(Response::View(ViewUpdate::Text(TextUpdate {
                node: NodeId(2),
                attempt: 1,
                offset: 12,
                segments: vec![
                    TextSegment::Reasoning {
                        text: "先分析调用链".to_owned(),
                    },
                    TextSegment::Text {
                        text: "结论如下".to_owned(),
                    },
                ],
            })))),
        },
        WireCase {
            name: "response/cancelled",
            frame: RootFrame::Response(Box::new(Response::Cancelled { owned: true })),
        },
        WireCase {
            name: "response/error/bad_request",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::BadRequest,
                "request or frozen configuration is invalid",
            )))),
        },
        WireCase {
            name: "response/error/not_found",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::NotFound,
                "session or Turn not found",
            )))),
        },
        WireCase {
            name: "response/error/conflict/turn_id_used",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::Conflict,
                message: "request identity or Session binding conflicts".to_owned(),
                conflict: Some(ConflictReason::TurnIdUsed),
                recovery: None,
                store: None,
            }))),
        },
        WireCase {
            name: "response/error/conflict/session_exists",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::Conflict,
                message: "request identity or Session binding conflicts".to_owned(),
                conflict: Some(ConflictReason::SessionExists),
                recovery: None,
                store: None,
            }))),
        },
        WireCase {
            name: "response/error/conflict/binding_mismatch",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::Conflict,
                message: "request identity or Session binding conflicts".to_owned(),
                conflict: Some(ConflictReason::BindingMismatch),
                recovery: None,
                store: None,
            }))),
        },
        WireCase {
            name: "response/error/conflict/settings_changed",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::Conflict,
                message: "session settings changed".to_owned(),
                conflict: Some(ConflictReason::SettingsChanged),
                recovery: None,
                store: None,
            }))),
        },
        WireCase {
            name: "response/error/busy",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::Busy,
                "session is busy",
            )))),
        },
        WireCase {
            name: "response/error/stopping",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::Stopping,
                "backend is stopping",
            )))),
        },
        WireCase {
            name: "response/error/store_unavailable/recovery_commit_failed_with_turn",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store recovery required".to_owned(),
                conflict: None,
                recovery: Some(RecoveryNeed {
                    turn: Some(TurnRef {
                        session_id: session_id_primary(),
                        turn_id: turn_id_primary(),
                    }),
                    cause: RecoveryCause::CommitFailed,
                }),
                store: None,
            }))),
        },
        WireCase {
            name: "response/error/store_unavailable/recovery_write_disabled",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store recovery required".to_owned(),
                conflict: None,
                recovery: Some(RecoveryNeed {
                    turn: None,
                    cause: RecoveryCause::WriteDisabled,
                }),
                store: None,
            }))),
        },
        WireCase {
            name: "response/error/store_unavailable/recovery_worker_failed",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store recovery required".to_owned(),
                conflict: None,
                recovery: Some(RecoveryNeed {
                    turn: None,
                    cause: RecoveryCause::WorkerFailed,
                }),
                store: None,
            }))),
        },
        WireCase {
            name: "response/error/store_unavailable/recovery_reopen_failed",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store recovery required".to_owned(),
                conflict: None,
                recovery: Some(RecoveryNeed {
                    turn: None,
                    cause: RecoveryCause::ReopenFailed,
                }),
                store: None,
            }))),
        },
        WireCase {
            name: "response/error/store_unavailable/not_initialized",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store unavailable".to_owned(),
                conflict: None,
                recovery: None,
                store: Some(Box::new(StoreDiagnostic::NotInitialized {
                    path: PathBuf::from("/var/lib/lattice/sessions.redb"),
                })),
            }))),
        },
        WireCase {
            name: "response/error/store_unavailable/legacy_stores_empty",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store unavailable".to_owned(),
                conflict: None,
                recovery: None,
                store: Some(Box::new(StoreDiagnostic::LegacyStores {
                    paths: Vec::new(),
                })),
            }))),
        },
        WireCase {
            name: "response/error/store_unavailable/legacy_stores_non_empty",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store unavailable".to_owned(),
                conflict: None,
                recovery: None,
                store: Some(Box::new(StoreDiagnostic::LegacyStores {
                    paths: vec![
                        PathBuf::from("/var/lib/lattice/v1.db"),
                        PathBuf::from("/var/lib/lattice/v2.db"),
                    ],
                })),
            }))),
        },
        WireCase {
            name: "response/error/store_unavailable/unsupported_version",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store unavailable".to_owned(),
                conflict: None,
                recovery: None,
                store: Some(Box::new(StoreDiagnostic::UnsupportedVersion {
                    path: PathBuf::from("/var/lib/lattice/sessions.redb"),
                    table: "sessions".to_owned(),
                    key: "schema_version".to_owned(),
                    found: 99,
                    supported: 1,
                })),
            }))),
        },
        WireCase {
            name: "response/error/store_unavailable/corruption",
            frame: RootFrame::Response(Box::new(Response::Error(LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: "session store unavailable".to_owned(),
                conflict: None,
                recovery: None,
                store: Some(Box::new(StoreDiagnostic::Corruption {
                    table: "nodes".to_owned(),
                    key: "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c:2".to_owned(),
                })),
            }))),
        },
        WireCase {
            name: "response/error/internal",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::Internal,
                "operation failed; inspect durable state",
            )))),
        },
        WireCase {
            name: "response/error/not_running",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::NotRunning,
                "local backend is not running",
            )))),
        },
        WireCase {
            name: "response/error/transport",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::Transport,
                "local connection ended before the message was complete",
            )))),
        },
        WireCase {
            name: "response/error/protocol",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::Protocol,
                "unsupported or malformed local protocol frame",
            )))),
        },
        WireCase {
            name: "response/error/permission",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::Permission,
                "local endpoint ownership or permissions rejected",
            )))),
        },
        WireCase {
            name: "response/error/already_running",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::AlreadyRunning,
                "local backend already owns STATE_ROOT",
            )))),
        },
        WireCase {
            name: "response/error/incompatible_backend",
            frame: RootFrame::Response(Box::new(Response::Error(plain_error(
                LocalErrorKind::IncompatibleBackend,
                "the running backend is newer than this client; use the newer binary",
            )))),
        },
    ]
}

fn verify_coverage(cases: &[WireCase]) {
    let mut tracker = CoverageTracker::default();
    for case in cases {
        tracker.observe_case(case);
    }

    let expected_enums: &[(&str, &[&str])] = &[
        ("Request", REQUEST_VARIANTS),
        ("Response", RESPONSE_VARIANTS),
        ("LoginAction", LOGIN_ACTION_VARIANTS),
        ("LoginResponse", LOGIN_RESPONSE_VARIANTS),
        ("ConfigureSession", CONFIGURE_SESSION_VARIANTS),
        ("TurnLookup", TURN_LOOKUP_VARIANTS),
        ("Acceptance", ACCEPTANCE_VARIANTS),
        ("ViewUpdate", VIEW_UPDATE_VARIANTS),
        ("TextSegment", TEXT_SEGMENT_VARIANTS),
        ("LocalErrorKind", LOCAL_ERROR_KIND_VARIANTS),
        ("ConflictReason", CONFLICT_REASON_VARIANTS),
        ("RecoveryCause", RECOVERY_CAUSE_VARIANTS),
        ("StoreDiagnostic", STORE_DIAGNOSTIC_VARIANTS),
        ("AuthLoginOutcome", AUTH_LOGIN_OUTCOME_VARIANTS),
        ("AuthCatalogOutcome", AUTH_CATALOG_OUTCOME_VARIANTS),
        ("ModelCandidateQuery", MODEL_CANDIDATE_QUERY_VARIANTS),
        ("Availability", AVAILABILITY_VARIANTS),
        (
            "ConfigurationRequirement",
            CONFIGURATION_REQUIREMENT_VARIANTS,
        ),
        ("RevocationStatus", REVOCATION_STATUS_VARIANTS),
    ];

    // 样例中登记过的 enum 必须正好是清单里的这些：新增守卫时要同时登记进清单。
    let observed_enum_names: BTreeSet<&str> = tracker.enums.keys().copied().collect();
    let listed_enum_names: BTreeSet<&str> = expected_enums.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        observed_enum_names, listed_enum_names,
        "样例中登记的 enum 与核验清单不一致"
    );
    for (enum_name, all_variants) in expected_enums {
        let expected: BTreeSet<&str> = all_variants.iter().copied().collect();
        assert_eq!(
            expected.len(),
            all_variants.len(),
            "守卫 {enum_name} 的变体列表存在重复项"
        );
        let observed = tracker.enums.get(enum_name).cloned().unwrap_or_default();
        assert_eq!(
            observed, expected,
            "enum {enum_name} 的样例变体集合与守卫列出的全部变体不一致"
        );
    }

    let expected_options = [
        "Request::List.after",
        "ConfigureRequest.agent",
        "ConfigureRequest.model",
        "ConfigureRequest.effort",
        "TurnRequest.expected_settings",
        "ModelCandidateQuery::List.provider",
        "AuthRemoval.revocation",
        "AuthStatus.error",
        "ProviderAuthStatus.email",
        "ProviderAuthStatus.expires_at",
        "ModelCandidate.name",
        "ModelCandidate.context",
        "ModelCandidate.output",
        "ModelCandidate.cost",
        "ModelPrice.input",
        "ModelPrice.output",
        "ModelPrice.cache_read",
        "ModelPrice.cache_write",
        "ModelWarning.provider",
        "Response::Sessions.next",
        "SessionHead.model",
        "SessionHead.provider",
        "SessionHead.effort",
        "SessionHead.turn_id",
        "SessionHead.diagnostic",
        "ConfigureReply.adjustment",
        "EffortAdjustment.from",
        "EffortAdjustment.to",
        "LocalError.conflict",
        "LocalError.recovery",
        "LocalError.store",
        "RecoveryNeed.turn",
    ];
    // 追踪器登记过的 Option 字段必须正好是清单里的这些：新增 Option 字段要同时登记进清单。
    let recorded_options: BTreeSet<&str> = tracker.options.keys().copied().collect();
    let listed_options: BTreeSet<&str> = expected_options.iter().copied().collect();
    assert_eq!(
        recorded_options, listed_options,
        "追踪器登记的 Option 字段与核验清单不一致"
    );
    for field in expected_options {
        let (saw_some, saw_none) = tracker
            .options
            .get(field)
            .copied()
            .unwrap_or((false, false));
        assert!(
            saw_some && saw_none,
            "Option 字段 {field} 未同时覆盖 Some 与 None（Some={saw_some}, None={saw_none}）"
        );
    }

    let expected_collections = [
        "AuthStatus.providers",
        "ModelCandidates.models",
        "ModelCandidates.warnings",
        "ModelCandidate.supported_efforts",
        "Response::Sessions.items",
        "LocalHistory.entries",
        "TextUpdate.segments",
        "StoreDiagnostic::LegacyStores.paths",
    ];
    // 追踪器登记过的集合字段必须正好是清单里的这些：新增集合字段要同时登记进清单。
    let recorded_collections: BTreeSet<&str> = tracker.collections.keys().copied().collect();
    let listed_collections: BTreeSet<&str> = expected_collections.iter().copied().collect();
    assert_eq!(
        recorded_collections, listed_collections,
        "追踪器登记的集合字段与核验清单不一致"
    );
    for field in expected_collections {
        let (saw_empty, saw_non_empty) = tracker
            .collections
            .get(field)
            .copied()
            .unwrap_or((false, false));
        assert!(
            saw_empty && saw_non_empty,
            "集合字段 {field} 未同时覆盖空与非空（empty={saw_empty}, non_empty={saw_non_empty}）"
        );
    }
}

fn encode_all_cases(cases: &[WireCase]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for case in cases {
        assert!(!case.name.is_empty(), "样例名不能为空");
        let encoded = case.encode_and_round_trip();
        let previous = map.insert(case.name.to_owned(), encoded);
        assert!(
            previous.is_none(),
            "存在重复的线上形状样例名：{}",
            case.name
        );
    }
    map
}

fn snapshot_file_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_RELATIVE_PATH)
}

#[test]
fn wire_shape_gate_covers_version_and_update_matrix() {
    let mut baseline_cases = BTreeMap::new();
    baseline_cases.insert(
        "request/status".to_owned(),
        "{\"operation\":\"status\"}".to_owned(),
    );
    baseline_cases.insert(
        "request/stop".to_owned(),
        "{\"operation\":\"stop\"}".to_owned(),
    );

    let mut changed_cases = baseline_cases.clone();
    changed_cases.insert(
        "request/status".to_owned(),
        "{\"operation\":\"daemon_status\"}".to_owned(),
    );
    changed_cases.insert(
        "request/extra".to_owned(),
        "{\"operation\":\"extra\"}".to_owned(),
    );

    let stored_v63 = WireShapeSnapshot {
        protocol_version: 63,
        cases: baseline_cases.clone(),
    };

    // 1. 文件不存在：未请求更新时失败；请求更新时写入。
    assert!(
        matches!(
            decide_snapshot(None, 63, &baseline_cases, false),
            GateDecision::Fail(ref msg) if msg.contains("不存在")
        ),
        "文件不存在且未请求更新时必须失败"
    );
    assert_eq!(
        decide_snapshot(None, 63, &baseline_cases, true),
        GateDecision::Write(WireShapeSnapshot {
            protocol_version: 63,
            cases: baseline_cases.clone(),
        }),
        "文件不存在且请求更新时必须写入当前版本与样例"
    );

    // 2. 版本相同（63 == 63）、内容相同：通过，不重写文件（无论是否请求更新）。
    assert_eq!(
        decide_snapshot(Some(&stored_v63), 63, &baseline_cases, false),
        GateDecision::Pass,
        "版本与内容都相同时必须通过"
    );
    assert_eq!(
        decide_snapshot(Some(&stored_v63), 63, &baseline_cases, true),
        GateDecision::Pass,
        "版本与内容都相同时即使带更新请求也不应重写文件"
    );

    // 3. 版本相同（63 == 63）、内容不同：失败，即使请求更新也失败，并提示升版本与不一致样例名。
    for update_requested in [false, true] {
        let decision = decide_snapshot(Some(&stored_v63), 63, &changed_cases, update_requested);
        let GateDecision::Fail(reason) = decision else {
            panic!("同版本形状变化时（update={update_requested}）必须失败，实际为 {decision:?}");
        };
        assert!(
            reason.contains("线上形状变了") && reason.contains("PROTOCOL_VERSION"),
            "失败原因必须说明线上形状变了并要求先升 PROTOCOL_VERSION：{reason}"
        );
        assert!(
            reason.contains("request/extra") && reason.contains("request/status"),
            "失败原因必须列出不一致的样例名：{reason}"
        );
    }

    // 4. 当前版本大于文件版本（64 > 63，以及 63 > 62）：未请求更新时失败；请求更新时写入。
    let stored_v62 = WireShapeSnapshot {
        protocol_version: 62,
        cases: baseline_cases.clone(),
    };
    assert!(
        matches!(
            decide_snapshot(Some(&stored_v62), 63, &changed_cases, false),
            GateDecision::Fail(_)
        ),
        "当前版本 63 大于文件版本 62 且未请求更新时必须失败"
    );
    assert_eq!(
        decide_snapshot(Some(&stored_v62), 63, &changed_cases, true),
        GateDecision::Write(WireShapeSnapshot {
            protocol_version: 63,
            cases: changed_cases.clone(),
        }),
        "当前版本 63 大于文件版本 62 且请求更新时必须写入新快照"
    );
    assert!(
        matches!(
            decide_snapshot(Some(&stored_v63), 64, &changed_cases, false),
            GateDecision::Fail(_)
        ),
        "当前版本 64 大于文件版本 63 且未请求更新时必须失败"
    );
    assert_eq!(
        decide_snapshot(Some(&stored_v63), 64, &changed_cases, true),
        GateDecision::Write(WireShapeSnapshot {
            protocol_version: 64,
            cases: changed_cases.clone(),
        }),
        "当前版本 64 大于文件版本 63 且请求更新时必须写入新快照"
    );

    // 5. 当前版本小于文件版本（62 < 63，以及 63 < 64）：无论是否请求更新都失败。
    let stored_v64 = WireShapeSnapshot {
        protocol_version: 64,
        cases: baseline_cases.clone(),
    };
    for update_requested in [false, true] {
        assert!(
            matches!(
                decide_snapshot(Some(&stored_v63), 62, &baseline_cases, update_requested),
                GateDecision::Fail(_)
            ),
            "当前版本 62 小于文件版本 63 时（update={update_requested}）必须失败"
        );
        assert!(
            matches!(
                decide_snapshot(Some(&stored_v64), 63, &baseline_cases, update_requested),
                GateDecision::Fail(_)
            ),
            "当前版本 63 小于文件版本 64 时（update={update_requested}）必须失败"
        );
    }
}

#[test]
fn wire_shape_matches_protocol_baseline() {
    let cases = build_wire_cases();
    verify_coverage(&cases);
    let current_cases = encode_all_cases(&cases);

    let path = snapshot_file_path();
    let stored = match std::fs::read_to_string(&path) {
        Ok(raw) => {
            assert!(
                raw.ends_with('\n') && !raw.ends_with("\n\n"),
                "快照文件 {} 必须以单个换行符结尾",
                path.display()
            );
            let parsed: WireShapeSnapshot = serde_json::from_str(&raw)
                .unwrap_or_else(|err| panic!("解析快照文件 {} 失败：{err}", path.display()));
            Some(parsed)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => panic!("读取快照文件 {} 失败：{err}", path.display()),
    };

    let update_requested = std::env::var_os(UPDATE_ENV_VAR).is_some_and(|v| v == "1");
    match decide_snapshot(
        stored.as_ref(),
        PROTOCOL_VERSION,
        &current_cases,
        update_requested,
    ) {
        GateDecision::Pass => {}
        GateDecision::Write(snapshot) => {
            let mut formatted =
                serde_json::to_string_pretty(&snapshot).expect("美化序列化线上形状快照");
            formatted.push('\n');
            std::fs::write(&path, formatted)
                .unwrap_or_else(|err| panic!("写入快照文件 {} 失败：{err}", path.display()));
        }
        GateDecision::Fail(reason) => panic!("{reason}"),
    }
}
