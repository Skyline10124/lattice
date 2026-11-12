// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-api` Local IPC 根帧、登录帧、结构体片段以及承载 8 个一致性场景的响应帧。

use std::path::PathBuf;

use lattice_conversation::{Effort, SessionId};
use lattice_llm::{ModelCandidateQuery, auth::RevocationStatus};
use lattice_session::store::{NodeId, Page};

use super::{
    CaseBook, agent_coder, auth_key, model_gpt5, model_ref_gpt5,
    models::{auth_status_full, model_candidates_full, provider_auth_status_full},
    non_utf8_workspace_path, provider_openai,
    session::{
        scenario_cancelled, scenario_indeterminate_no_result, scenario_orphan_executing,
        scenario_orphan_requesting, scenario_orphan_reserved, scenario_retry_then_success,
        scenario_success_tool_then_finish, scenario_tool_failed, session_record_at,
        store_session_list_row_full, store_session_list_row_minimal,
    },
    session_id_primary, session_id_secondary, settings_version_fixed, ts_created, ts_ended,
    turn_id_1, turn_id_2, utf8_store_path,
};
use crate::{
    local::{
        Acceptance, AuthCatalogOutcome, AuthLoggedOut, AuthLoginOutcome, AuthRemoval,
        ConfigureReply, ConfigureRequest, ConfigureSession, ConflictReason, DaemonStatus,
        EffortAdjustment, LocalError, LocalErrorKind, LocalHistory, RecoveryCause, RecoveryNeed,
        Request, Response, SessionHead, SessionListRow, StoreDiagnostic, TextSegment, TextUpdate,
        TurnLookup, TurnRequest, TurnView, ViewUpdate,
        auth::login::{LoginAction, LoginResponse},
    },
    turn::TurnRef,
};

fn daemon_status_sample() -> DaemonStatus {
    DaemonStatus {
        epoch: "epoch-0001".to_owned(),
        stopping: false,
        owners: 1,
        unconfirmed_operations: 0,
    }
}

fn local_error_minimal() -> LocalError {
    LocalError {
        kind: LocalErrorKind::Busy,
        message: "session is busy".to_owned(),
        conflict: None,
        recovery: None,
        store: None,
    }
}

fn local_error_conflict() -> LocalError {
    LocalError {
        kind: LocalErrorKind::Conflict,
        message: "request identity or Session binding conflicts".to_owned(),
        conflict: Some(ConflictReason::TurnIdUsed),
        recovery: None,
        store: None,
    }
}

fn recovery_need_full() -> RecoveryNeed {
    RecoveryNeed {
        turn: Some(TurnRef {
            session_id: session_id_primary(),
            turn_id: turn_id_2(),
        }),
        cause: RecoveryCause::CommitFailed,
    }
}

fn recovery_need_minimal() -> RecoveryNeed {
    RecoveryNeed {
        turn: None,
        cause: RecoveryCause::WorkerFailed,
    }
}

fn local_error_recovery() -> LocalError {
    LocalError {
        kind: LocalErrorKind::StoreUnavailable,
        message: "session store recovery required".to_owned(),
        conflict: None,
        recovery: Some(recovery_need_full()),
        store: None,
    }
}

fn local_error_store() -> LocalError {
    LocalError {
        kind: LocalErrorKind::StoreUnavailable,
        message: "session store unavailable".to_owned(),
        conflict: None,
        recovery: None,
        store: Some(Box::new(StoreDiagnostic::Corruption {
            table: "nodes".to_owned(),
            key: format!("{}/2", session_id_primary()),
        })),
    }
}

fn session_head_full() -> SessionHead {
    SessionHead {
        session_id: session_id_primary(),
        position: NodeId(2),
        agent: "coder".to_owned(),
        model: Some(model_gpt5()),
        provider: Some(provider_openai()),
        effort: Some(Effort::High),
        settings_version: settings_version_fixed(),
        turn_id: Some(turn_id_2()),
        owner: true,
        diagnostic: Some(local_error_recovery()),
        path_replaced: true,
    }
}

fn session_head_minimal() -> SessionHead {
    SessionHead {
        session_id: session_id_primary(),
        position: NodeId(0),
        agent: "coder".to_owned(),
        model: None,
        provider: None,
        effort: None,
        settings_version: settings_version_fixed(),
        turn_id: None,
        owner: false,
        diagnostic: None,
        path_replaced: false,
    }
}

fn configure_request_full() -> ConfigureRequest {
    ConfigureRequest {
        session: ConfigureSession::Existing(session_id_primary()),
        workspace: non_utf8_workspace_path(),
        agent: Some(agent_coder()),
        model: Some(model_ref_gpt5()),
        effort: Some(Effort::High),
    }
}

fn configure_request_minimal() -> ConfigureRequest {
    ConfigureRequest {
        session: ConfigureSession::New(session_id_primary()),
        workspace: non_utf8_workspace_path(),
        agent: None,
        model: None,
        effort: None,
    }
}

fn turn_request_full() -> TurnRequest {
    TurnRequest {
        turn_id: turn_id_1(),
        session: session_id_primary(),
        workspace: non_utf8_workspace_path(),
        input: "请查看 Cargo.toml 的行数".to_owned(),
        expected_settings: Some(settings_version_fixed()),
    }
}

fn turn_request_minimal() -> TurnRequest {
    TurnRequest {
        turn_id: turn_id_1(),
        session: session_id_primary(),
        workspace: non_utf8_workspace_path(),
        input: "请查看 Cargo.toml 的行数".to_owned(),
        expected_settings: None,
    }
}

pub(super) fn register(book: &mut CaseBook) {
    let success = scenario_success_tool_then_finish();
    let retried = scenario_retry_then_success();
    let tool_failed = scenario_tool_failed();
    let cancelled = scenario_cancelled();
    let orphan_reserved = scenario_orphan_reserved();
    let orphan_requesting = scenario_orphan_requesting();
    let orphan_executing = scenario_orphan_executing();
    let indeterminate = scenario_indeterminate_no_result();

    enum_fragments!(
        book,
        "configure_session",
        ConfigureSession {
            ("new", ConfigureSession::New(_) => ConfigureSession::New(session_id_primary())),
            ("existing", ConfigureSession::Existing(_) => ConfigureSession::Existing(session_id_primary())),
        }
    );

    enum_fragments!(
        book,
        "turn_lookup",
        TurnLookup {
            (
                "accepted",
                TurnLookup::Accepted { .. } => TurnLookup::Accepted {
                    turn: success.turn_detail.clone(),
                }
            ),
            ("in_flight", TurnLookup::InFlight => TurnLookup::InFlight),
        }
    );

    enum_fragments!(
        book,
        "acceptance",
        Acceptance {
            (
                "accepted",
                Acceptance::Accepted { .. } => Acceptance::Accepted {
                    turn: TurnView::from(&orphan_reserved.turn_detail),
                }
            ),
            (
                "confirmation_unknown",
                Acceptance::ConfirmationUnknown { .. } => Acceptance::ConfirmationUnknown {
                    turn_id: turn_id_1(),
                    session_id: session_id_primary(),
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "text_segment",
        TextSegment {
            (
                "text",
                TextSegment::Text { .. } => TextSegment::Text {
                    text: "Cargo.toml 共包含 42 行配置。".to_owned(),
                }
            ),
            (
                "reasoning",
                TextSegment::Reasoning { .. } => TextSegment::Reasoning {
                    text: "先核对工作区成员列表".to_owned(),
                }
            ),
        }
    );

    let text_update_full = TextUpdate {
        node: NodeId(2),
        attempt: 0,
        offset: 0,
        segments: vec![
            TextSegment::Reasoning {
                text: "先读取 Cargo.toml".to_owned(),
            },
            TextSegment::Text {
                text: "已定位配置文件。".to_owned(),
            },
        ],
    };
    let text_update_minimal = TextUpdate {
        node: NodeId(2),
        attempt: 0,
        offset: 18,
        segments: Vec::new(),
    };

    enum_fragments!(
        book,
        "view_update",
        ViewUpdate {
            ("head", ViewUpdate::Head(_) => ViewUpdate::Head(session_head_full())),
            (
                "entry",
                ViewUpdate::Entry { .. } => ViewUpdate::Entry {
                    entry: Box::new(success.entries[1].clone()),
                }
            ),
            ("text", ViewUpdate::Text(_) => ViewUpdate::Text(text_update_full.clone())),
        }
    );

    enum_fragments!(
        book,
        "local_error_kind",
        LocalErrorKind {
            ("bad_request", LocalErrorKind::BadRequest => LocalErrorKind::BadRequest),
            ("not_found", LocalErrorKind::NotFound => LocalErrorKind::NotFound),
            ("conflict", LocalErrorKind::Conflict => LocalErrorKind::Conflict),
            ("busy", LocalErrorKind::Busy => LocalErrorKind::Busy),
            ("stopping", LocalErrorKind::Stopping => LocalErrorKind::Stopping),
            ("store_unavailable", LocalErrorKind::StoreUnavailable => LocalErrorKind::StoreUnavailable),
            ("internal", LocalErrorKind::Internal => LocalErrorKind::Internal),
            ("not_running", LocalErrorKind::NotRunning => LocalErrorKind::NotRunning),
            ("transport", LocalErrorKind::Transport => LocalErrorKind::Transport),
            ("protocol", LocalErrorKind::Protocol => LocalErrorKind::Protocol),
            ("permission", LocalErrorKind::Permission => LocalErrorKind::Permission),
            ("already_running", LocalErrorKind::AlreadyRunning => LocalErrorKind::AlreadyRunning),
            ("incompatible_backend", LocalErrorKind::IncompatibleBackend => LocalErrorKind::IncompatibleBackend),
        }
    );

    enum_fragments!(
        book,
        "conflict_reason",
        ConflictReason {
            ("turn_id_used", ConflictReason::TurnIdUsed => ConflictReason::TurnIdUsed),
            ("session_exists", ConflictReason::SessionExists => ConflictReason::SessionExists),
            ("binding_mismatch", ConflictReason::BindingMismatch => ConflictReason::BindingMismatch),
            ("settings_changed", ConflictReason::SettingsChanged => ConflictReason::SettingsChanged),
        }
    );

    enum_fragments!(
        book,
        "recovery_cause",
        RecoveryCause {
            ("commit_failed", RecoveryCause::CommitFailed => RecoveryCause::CommitFailed),
            ("write_disabled", RecoveryCause::WriteDisabled => RecoveryCause::WriteDisabled),
            ("worker_failed", RecoveryCause::WorkerFailed => RecoveryCause::WorkerFailed),
            ("reopen_failed", RecoveryCause::ReopenFailed => RecoveryCause::ReopenFailed),
        }
    );

    enum_fragments!(
        book,
        "store_diagnostic",
        StoreDiagnostic {
            (
                "not_initialized",
                StoreDiagnostic::NotInitialized { .. } => StoreDiagnostic::NotInitialized {
                    path: utf8_store_path(),
                }
            ),
            (
                "legacy_stores",
                StoreDiagnostic::LegacyStores { .. } => StoreDiagnostic::LegacyStores {
                    paths: vec![
                        PathBuf::from("/var/lib/lattice/legacy-v1.redb"),
                        PathBuf::from("/var/lib/lattice/legacy-v2.redb"),
                    ],
                }
            ),
            (
                "unsupported_version",
                StoreDiagnostic::UnsupportedVersion { .. } => StoreDiagnostic::UnsupportedVersion {
                    path: utf8_store_path(),
                    table: "sessions".to_owned(),
                    key: "schema_version".to_owned(),
                    found: 99,
                    supported: 1,
                }
            ),
            (
                "corruption",
                StoreDiagnostic::Corruption { .. } => StoreDiagnostic::Corruption {
                    table: "nodes".to_owned(),
                    key: format!("{}/2", session_id_primary()),
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "auth_catalog_outcome",
        AuthCatalogOutcome {
            (
                "refreshed",
                AuthCatalogOutcome::Refreshed { .. } => AuthCatalogOutcome::Refreshed {
                    model_count: 12,
                }
            ),
            (
                "failed",
                AuthCatalogOutcome::Failed { .. } => AuthCatalogOutcome::Failed {
                    warning: "catalog endpoint timed out".to_owned(),
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "auth_login_outcome",
        AuthLoginOutcome {
            (
                "signed_in",
                AuthLoginOutcome::SignedIn { .. } => AuthLoginOutcome::SignedIn {
                    status: provider_auth_status_full(),
                    catalog: AuthCatalogOutcome::Refreshed { model_count: 12 },
                }
            ),
            ("cancelled", AuthLoginOutcome::Cancelled {} => AuthLoginOutcome::Cancelled {}),
        }
    );

    enum_fragments!(
        book,
        "login_action",
        LoginAction {
            (
                "callback",
                LoginAction::Callback { .. } => LoginAction::Callback {
                    url: auth_key("http://127.0.0.1:1455/auth/callback?code=ac_01&state=st_01"),
                }
            ),
            ("cancel", LoginAction::Cancel {} => LoginAction::Cancel {}),
        }
    );

    enum_fragments!(
        book,
        "login_response",
        LoginResponse {
            (
                "auth_login_authorization",
                LoginResponse::AuthLoginAuthorization { .. } => LoginResponse::AuthLoginAuthorization {
                    authorization_url: auth_key("https://auth.openai.com/authorize?state=st_01"),
                }
            ),
            (
                "auth_login_result",
                LoginResponse::AuthLoginResult(_) => LoginResponse::AuthLoginResult(
                    AuthLoginOutcome::SignedIn {
                        status: provider_auth_status_full(),
                        catalog: AuthCatalogOutcome::Failed {
                            warning: "catalog endpoint timed out".to_owned(),
                        },
                    }
                )
            ),
            ("error", LoginResponse::Error(_) => LoginResponse::Error(local_error_minimal())),
        }
    );

    enum_fragments!(
        book,
        "request",
        Request {
            ("status", Request::Status => Request::Status),
            ("stop", Request::Stop => Request::Stop),
            (
                "auth_set",
                Request::AuthSet { .. } => Request::AuthSet {
                    provider: "openai".to_owned(),
                    key: auth_key("sk-test-fixed-fixture-key"),
                }
            ),
            (
                "auth_remove",
                Request::AuthRemove { .. } => Request::AuthRemove {
                    provider: "openai".to_owned(),
                }
            ),
            (
                "auth_login",
                Request::AuthLogin { .. } => Request::AuthLogin {
                    provider: "chatgpt".to_owned(),
                }
            ),
            (
                "auth_logout",
                Request::AuthLogout { .. } => Request::AuthLogout {
                    provider: "chatgpt".to_owned(),
                }
            ),
            ("auth_status", Request::AuthStatus {} => Request::AuthStatus {}),
            (
                "models",
                Request::Models { .. } => Request::Models {
                    query: ModelCandidateQuery::List {
                        provider: Some(provider_openai()),
                        ready_only: true,
                    },
                }
            ),
            ("models_refresh", Request::ModelsRefresh {} => Request::ModelsRefresh {}),
            (
                "resolve_session",
                Request::ResolveSession { .. } => Request::ResolveSession {
                    prefix: "3f8b2c1a".to_owned(),
                }
            ),
            (
                "list",
                Request::List { .. } => Request::List {
                    after: Some(session_id_primary()),
                    limit: 20,
                }
            ),
            (
                "log",
                Request::Log { .. } => Request::Log {
                    session_id: session_id_primary(),
                }
            ),
            (
                "usage",
                Request::Usage { .. } => Request::Usage {
                    session_id: session_id_primary().to_string(),
                }
            ),
            (
                "turn",
                Request::Turn { .. } => Request::Turn {
                    turn_id: turn_id_1(),
                }
            ),
            (
                "head",
                Request::Head { .. } => Request::Head {
                    session_id: session_id_primary(),
                }
            ),
            (
                "history",
                Request::History { .. } => Request::History {
                    session_id: session_id_primary(),
                    before: NodeId(3),
                    turns: 10,
                }
            ),
            (
                "rewind",
                Request::Rewind { .. } => Request::Rewind {
                    session_id: session_id_primary(),
                    target: NodeId(0),
                    workspace: non_utf8_workspace_path(),
                }
            ),
            (
                "configure",
                Request::Configure { .. } => Request::Configure {
                    request: configure_request_full(),
                }
            ),
            (
                "submit",
                Request::Submit { .. } => Request::Submit {
                    request: turn_request_full(),
                }
            ),
            (
                "cancel",
                Request::Cancel { .. } => Request::Cancel {
                    turn_id: turn_id_1(),
                }
            ),
            (
                "watch",
                Request::Watch { .. } => Request::Watch {
                    session_id: session_id_primary(),
                }
            ),
        }
    );

    let auth_removal_full = AuthRemoval {
        removed: true,
        revocation: Some(RevocationStatus::Confirmed),
    };
    let auth_removal_minimal = AuthRemoval {
        removed: false,
        revocation: None,
    };

    let session_list_row_full = SessionListRow {
        session: store_session_list_row_full(),
        owner: true,
    };
    let session_list_row_minimal = SessionListRow {
        session: store_session_list_row_minimal(),
        owner: false,
    };

    let session_page_full: Page<SessionListRow, SessionId> = Page {
        items: vec![
            session_list_row_full.clone(),
            session_list_row_minimal.clone(),
        ],
        next: Some(session_id_secondary()),
    };
    let session_page_minimal: Page<SessionListRow, SessionId> = Page {
        items: Vec::new(),
        next: None,
    };

    let effort_adjustment_full = EffortAdjustment {
        from: Some(Effort::High),
        to: Some(Effort::Low),
    };
    let effort_adjustment_from_none = EffortAdjustment {
        from: None,
        to: Some(Effort::Low),
    };
    let effort_adjustment_to_none = EffortAdjustment {
        from: Some(Effort::High),
        to: None,
    };

    let configure_reply_full = ConfigureReply {
        head: session_head_full(),
        adjustment: Some(effort_adjustment_full),
    };
    let configure_reply_minimal = ConfigureReply {
        head: session_head_minimal(),
        adjustment: None,
    };

    let local_history_full = LocalHistory {
        entries: success.entries.clone(),
    };
    let local_history_minimal = LocalHistory {
        entries: Vec::new(),
    };

    enum_fragments!(
        book,
        "response",
        Response {
            ("status", Response::Status(_) => Response::Status(daemon_status_sample())),
            ("auth_updated", Response::AuthUpdated => Response::AuthUpdated),
            ("auth_removed", Response::AuthRemoved(_) => Response::AuthRemoved(auth_removal_full.clone())),
            (
                "auth_logged_out",
                Response::AuthLoggedOut(_) => Response::AuthLoggedOut(AuthLoggedOut {
                    revocation: RevocationStatus::Confirmed,
                })
            ),
            ("auth_status", Response::AuthStatus(_) => Response::AuthStatus(auth_status_full())),
            ("models", Response::Models(_) => Response::Models(model_candidates_full())),
            ("models_refreshed", Response::ModelsRefreshed => Response::ModelsRefreshed),
            (
                "session",
                Response::Session(_) => Response::Session(session_record_at(NodeId(3), ts_ended()))
            ),
            ("sessions", Response::Sessions(_) => Response::Sessions(session_page_full.clone())),
            ("log", Response::Log(_) => Response::Log(success.session_log.clone())),
            ("usage", Response::Usage(_) => Response::Usage(Box::new(success.session_usage.clone()))),
            (
                "turn",
                Response::Turn(_) => Response::Turn(TurnLookup::Accepted {
                    turn: success.turn_detail.clone(),
                })
            ),
            ("head", Response::Head(_) => Response::Head(session_head_full())),
            ("history", Response::History(_) => Response::History(local_history_full.clone())),
            (
                "rewind",
                Response::Rewind(_) => Response::Rewind(session_record_at(NodeId(0), ts_created()))
            ),
            ("configure", Response::Configure(_) => Response::Configure(configure_reply_full.clone())),
            (
                "acceptance",
                Response::Acceptance(_) => Response::Acceptance(Acceptance::Accepted {
                    turn: TurnView::from(&orphan_reserved.turn_detail),
                })
            ),
            (
                "view",
                Response::View(_) => Response::View(ViewUpdate::Entry {
                    entry: Box::new(success.entries[1].clone()),
                })
            ),
            ("cancelled", Response::Cancelled { .. } => Response::Cancelled { owned: true }),
            ("error", Response::Error(_) => Response::Error(local_error_conflict())),
        }
    );

    // 结构体形态片段
    book.insert_struct_pair("auth_removal", &auth_removal_full, &auth_removal_minimal);
    book.insert_struct_pair(
        "session_head",
        &session_head_full(),
        &session_head_minimal(),
    );
    book.insert_struct_pair("text_update", &text_update_full, &text_update_minimal);
    book.insert_struct_pair("local_history", &local_history_full, &local_history_minimal);
    book.insert_struct_pair(
        "session_list_row",
        &session_list_row_full,
        &session_list_row_minimal,
    );
    book.insert_struct_pair("session_page", &session_page_full, &session_page_minimal);
    book.insert_struct_pair(
        "recovery_need",
        &recovery_need_full(),
        &recovery_need_minimal(),
    );

    // LocalError 的三个可选载荷在生产中按错误种类互斥，分别给出各自真实可达形态。
    book.insert("fragment/local_error/minimal", &local_error_minimal());
    book.insert("fragment/local_error/conflict", &local_error_conflict());
    book.insert("fragment/local_error/recovery", &local_error_recovery());
    book.insert("fragment/local_error/store", &local_error_store());

    book.insert_struct_pair(
        "configure_request",
        &configure_request_full(),
        &configure_request_minimal(),
    );

    // EffortAdjustment 仅在 `from != to` 时出现，不存在两者同为 None 的真实状态。
    book.insert("fragment/effort_adjustment/full", &effort_adjustment_full);
    book.insert(
        "fragment/effort_adjustment/from_none",
        &effort_adjustment_from_none,
    );
    book.insert(
        "fragment/effort_adjustment/to_none",
        &effort_adjustment_to_none,
    );

    book.insert_struct_pair(
        "configure_reply",
        &configure_reply_full,
        &configure_reply_minimal,
    );
    book.insert_struct_pair(
        "turn_request",
        &turn_request_full(),
        &turn_request_minimal(),
    );

    // 内部一致场景根帧：每个场景同时驱动 Turn、History、View、Log、Usage、Acceptance 等根帧。
    for (scenario_slug, built) in [
        ("success_tool_then_finish", &success),
        ("retry_then_success", &retried),
        ("tool_failed", &tool_failed),
        ("cancelled", &cancelled),
        ("orphan_reserved", &orphan_reserved),
        ("orphan_requesting", &orphan_requesting),
        ("orphan_executing", &orphan_executing),
        ("indeterminate_no_result", &indeterminate),
    ] {
        book.insert(
            format!("frame/response/turn/{scenario_slug}"),
            &Response::Turn(TurnLookup::Accepted {
                turn: built.turn_detail.clone(),
            }),
        );
        book.insert(
            format!("frame/response/history/{scenario_slug}"),
            &Response::History(LocalHistory {
                entries: built.entries.clone(),
            }),
        );
        book.insert(
            format!("frame/response/log/{scenario_slug}"),
            &Response::Log(built.session_log.clone()),
        );
        book.insert(
            format!("frame/response/usage/{scenario_slug}"),
            &Response::Usage(Box::new(built.session_usage.clone())),
        );
    }

    book.insert(
        "frame/response/turn/in_flight",
        &Response::Turn(TurnLookup::InFlight),
    );
    book.insert(
        "frame/response/view/entry_user_message",
        &Response::View(ViewUpdate::Entry {
            entry: Box::new(success.entries[0].clone()),
        }),
    );
    book.insert(
        "frame/response/view/entry_step_continue",
        &Response::View(ViewUpdate::Entry {
            entry: Box::new(success.entries[1].clone()),
        }),
    );
    book.insert(
        "frame/response/view/entry_step_finish",
        &Response::View(ViewUpdate::Entry {
            entry: Box::new(success.entries[2].clone()),
        }),
    );
    book.insert(
        "frame/response/view/entry_step_tool_failed",
        &Response::View(ViewUpdate::Entry {
            entry: Box::new(tool_failed.entries[1].clone()),
        }),
    );
    book.insert(
        "frame/response/view/entry_step_cancelled",
        &Response::View(ViewUpdate::Entry {
            entry: Box::new(cancelled.entries[1].clone()),
        }),
    );
    book.insert(
        "frame/response/view/entry_step_orphan_requesting",
        &Response::View(ViewUpdate::Entry {
            entry: Box::new(orphan_requesting.entries[1].clone()),
        }),
    );
    book.insert(
        "frame/response/view/entry_step_indeterminate_no_result",
        &Response::View(ViewUpdate::Entry {
            entry: Box::new(indeterminate.entries[1].clone()),
        }),
    );
    book.insert(
        "frame/response/view/head_active_turn",
        &Response::View(ViewUpdate::Head(session_head_full())),
    );
    book.insert(
        "frame/response/view/text_delta",
        &Response::View(ViewUpdate::Text(text_update_full)),
    );
    book.insert(
        "frame/response/acceptance/accepted_reserved",
        &Response::Acceptance(Acceptance::Accepted {
            turn: TurnView::from(&orphan_reserved.turn_detail),
        }),
    );
    book.insert(
        "frame/response/acceptance/accepted_completed",
        &Response::Acceptance(Acceptance::Accepted {
            turn: TurnView::from(&success.turn_detail),
        }),
    );
    book.insert(
        "frame/response/acceptance/confirmation_unknown",
        &Response::Acceptance(Acceptance::ConfirmationUnknown {
            turn_id: turn_id_1(),
            session_id: session_id_primary(),
        }),
    );
    book.insert(
        "frame/request/list_first_page",
        &Request::List {
            after: None,
            limit: 20,
        },
    );
    book.insert(
        "frame/request/models_all",
        &Request::Models {
            query: ModelCandidateQuery::List {
                provider: None,
                ready_only: false,
            },
        },
    );
    book.insert(
        "frame/login/outcome_cancelled",
        &LoginResponse::AuthLoginResult(AuthLoginOutcome::Cancelled {}),
    );
}
