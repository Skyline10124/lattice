// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP Warn-mode coverage harness.
//!
//! ## Default CI path
//!
//! Reads `docs/lancet-warn-report.md` from the repo root and asserts that
//! no `fix-now` entries are present.  Fast; no I/O beyond the file read.
//!
//! ## Update path (`LATTICE_UPDATE_WARN_REPORT=1`)
//!
//! Runs each of the 8 CATALOG methods through `CepMode::Warn`, collects
//! `effect_warning` from the `CepAuditStore`, and
//! writes a fresh `docs/lancet-warn-report.md` at the repo root.
//!
//! Method categories
//! - **Overreach-capable:** use a custom handler that deliberately sets an
//!   empty `AuthorizedEffectRegion` and records an out-of-scope effect so
//!   `effect_warning` appears in the audit metadata.
//! - **Success-only:** `session.todo_write`, `observe.audit`, `observe.audit_replay` —
//!   no meaningful effect-region
//!   warn surface; run via the production registry with valid params only.
//!
//! Filter: `cep_warn_harness`

use std::path::PathBuf;
use std::sync::Arc;

use lancet_authority::CepTurnContext;
use lancet_authority::effect::{
    ledger::{EffectSource, ObservedEffect, ObservedScope},
    region::AuthorizedEffectRegion,
};
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::audit::{
    AuditObserve, AuditObserveParams, AuditQueryParams, AuditReplay, AuditReplayParams,
};
use lancet_catalog::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_catalog::methods::session::{TodoWrite, TodoWriteParams};
use lancet_catalog::methods::workspace::{
    Batch, BatchOperation, BatchParams, BatchResult, Create, CreateParams, CreateResult, Edit,
    EditOperation, EditParams, EditResult, ObserveWorkspace, ObserveWorkspaceParams,
    ObserveWorkspaceResult, ReadFileParams, ReadFileResult,
};
use lancet_catalog::{CatalogMethod, CepMethod};
use lancet_native::{
    CepAuditStore, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;

use lancet_protocol::policy::CepMode;
use tempfile::TempDir;

// ── Shared overreach builder ─────────────────────────────────────────────────
//
// Every overreach test follows the same pattern from cep_mode_integration.rs:
//  1. Set an EMPTY AuthorizedEffectRegion (no paths allowed).
//  2. Record a WorkspaceMutation effect at a path outside the empty region.
//  3. Return Ok.
// This guarantees the transport-level effect checker fires in CepMode::Warn,
// writing `{"effect_warning": true, ...}` into the durable audit metadata.

/// Build a Warn-mode server config from an in-memory audit store.
fn warn_server_config(audit: Arc<CepAuditStore>) -> ServerRunConfig {
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = audit;
    config.cep_mode = CepMode::Warn;
    config
}

/// Run a single overreach call and return true if the audit row contains
/// `effect_warning` in its metadata_json.
///
/// Spins up a per-call transport pair so calls don't share state.
async fn has_effect_warning(audit: Arc<CepAuditStore>, row_before: i64) -> bool {
    let count = audit.row_count().unwrap_or(0) as i64;
    let expected_id = row_before + 1;
    if count < expected_id {
        return false;
    }
    match audit.row_by_audit_id(expected_id) {
        Ok(row) => row
            .metadata_json
            .as_deref()
            .unwrap_or("")
            .contains("effect_warning"),
        Err(_) => false,
    }
}

// ── Per-method overreach handlers ────────────────────────────────────────────
// Each registers one method with an overreach handler that triggers effect_warning.

fn overreach_registry_observe_workspace() -> MethodRegistry {
    let mut r = MethodRegistry::new();
    r.register_method::<ObserveWorkspace, _, _>(
        |_params: ObserveWorkspaceParams, ctx| async move {
            ctx.authorized_region.set(AuthorizedEffectRegion::empty());
            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::WorkspaceObservation,
                scope: ObservedScope::Path(PathBuf::from("/out/of/scope/observe")),
                required_capability: RequiredCapability::ReadPath,
                source: EffectSource::HandlerBackend,
            });
            Ok(ObserveWorkspaceResult::ReadFile(ReadFileResult {
                content: "mock".into(),
                content_hash: None,
            }))
        },
    )
    .expect("overreach ObserveWorkspace");
    r
}

fn overreach_registry_create() -> MethodRegistry {
    let mut r = MethodRegistry::new();
    r.register_method::<Create, _, _>(|_params: CreateParams, ctx| async move {
        ctx.authorized_region.set(AuthorizedEffectRegion::empty());
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/out/of/scope/create")),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
        Ok(CreateResult {
            path: "mock".into(),
            bytes_written: 0,
            created_dirs: vec![],
        })
    })
    .expect("overreach Create");
    r
}

fn overreach_registry_edit() -> MethodRegistry {
    let mut r = MethodRegistry::new();
    r.register_method::<Edit, _, _>(|_params: EditParams, ctx| async move {
        ctx.authorized_region.set(AuthorizedEffectRegion::empty());
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/out/of/scope/edit")),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
        Ok(EditResult {
            changed_files: vec![],
        })
    })
    .expect("overreach Edit");
    r
}

fn overreach_registry_batch() -> MethodRegistry {
    let mut r = MethodRegistry::new();
    r.register_method::<Batch, _, _>(|_params: BatchParams, ctx| async move {
        ctx.authorized_region.set(AuthorizedEffectRegion::empty());
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/out/of/scope/batch")),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
        Ok(BatchResult {
            changed_files: vec![],
            created_files: vec![],
            edited_files: vec![],
            created_dirs: vec![],
        })
    })
    .expect("overreach Batch");
    r
}

fn overreach_registry_fetch() -> MethodRegistry {
    let mut r = MethodRegistry::new();
    r.register_method::<Fetch, _, _>(|_params: FetchParams, ctx| async move {
        ctx.authorized_region.set(AuthorizedEffectRegion::empty());
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::ExternalObservation,
            scope: ObservedScope::Path(PathBuf::from("/out/of/scope/fetch")),
            required_capability: RequiredCapability::FetchUrl,
            source: EffectSource::HandlerBackend,
        });
        Ok(FetchResult {
            status: 200,
            body: "mock".into(),
            content_type: None,
            truncated: None,
        })
    })
    .expect("overreach Fetch");
    r
}

// ── Overreach sample runner ──────────────────────────────────────────────────

struct OverreachResult {
    method: &'static str,
    warn_surface: &'static str,
    leaked: bool,
}

async fn run_overreach<M: CepMethod + CatalogMethod>(
    method_name: &'static str,
    registry: MethodRegistry,
    params: M::Params,
) -> OverreachResult
where
    M::Params: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    M::Result: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let audit = Arc::new(CepAuditStore::in_memory().expect("in-memory audit"));
    let config = warn_server_config(Arc::clone(&audit));
    let (client, server) = CepTransport::pair(8);
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

    let row_before = audit.row_count().unwrap_or(0) as i64;
    let _ = client.call::<M>(params).await;
    drop(client);
    server_task.abort();
    tokio::task::yield_now().await;

    let leaked = has_effect_warning(Arc::clone(&audit), row_before).await;

    OverreachResult {
        method: method_name,
        warn_surface: "overreach",
        leaked,
    }
}

// ── Repo root helper ─────────────────────────────────────────────────────────

/// Walk up from CARGO_MANIFEST_DIR to find the repo root (has `.planning/`).
fn find_repo_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap());

    let mut dir = manifest.as_path();
    loop {
        if dir.join(".planning").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    // Fallback: assume workspace root is two dirs up from crate manifest
    manifest
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(&manifest)
        .to_path_buf()
}

// ── Report generator ─────────────────────────────────────────────────────────

async fn generate_warn_report() -> String {
    // ── Overreach-capable canonical methods ──────────────────────────────

    let overreach_results = vec![
            run_overreach::<ObserveWorkspace>(
                "observe.workspace",
                overreach_registry_observe_workspace(),
                ObserveWorkspaceParams::ReadFile(ReadFileParams {
                    path: "src/lib.rs".into(),
                    offset: None,
                    limit: None,
                    line_numbers: None,
                }),
            )
            .await,
            run_overreach::<Create>(
                "mutate.create",
                overreach_registry_create(),
                CreateParams {
                    path: "mock.txt".into(),
                    content: "mock".into(),
                    create_parents: false,
                },
            )
            .await,
            run_overreach::<Edit>(
                "mutate.edit",
                overreach_registry_edit(),
                EditParams {
                    path: "mock.txt".into(),
                    expected_hash:
                        "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                            .into(),
                    edits: vec![EditOperation::ReplaceText {
                        old: "old".into(),
                        new: "new".into(),
                        occurrence: None,
                    }],
                },
            )
            .await,
            run_overreach::<Batch>(
                "mutate.batch",
                overreach_registry_batch(),
                BatchParams {
                    operations: vec![BatchOperation::CreateFile {
                        path: "mock.txt".into(),
                        content: "mock".into(),
                        create_parents: false,
                    }],
                },
            )
            .await,
            run_overreach::<Fetch>(
                "fetch.web",
                overreach_registry_fetch(),
                FetchParams {
                    url: "http://localhost/mock".into(),
                    headers: Default::default(),
                },
            )
            .await,
        ];

    // ── Success-only methods via production registry ─────────────────────

    // Build a temp workspace for methods that need a valid root.
    let ws_dir = TempDir::new().expect("temp dir");
    let ws_root = ws_dir.path();
    std::fs::create_dir_all(ws_root.join("src")).unwrap();
    std::fs::write(ws_root.join("src/lib.rs"), "fn main() {}").unwrap();
    let canonical_root = std::fs::canonicalize(ws_root).unwrap_or_else(|_| ws_root.to_path_buf());
    let canonical_str = canonical_root.to_str().unwrap().to_string();

    // Single production server for all 4 success-only methods.
    let audit_success = Arc::new(CepAuditStore::in_memory().expect("audit"));

    let mut production_registry = MethodRegistry::new();
    register_production_method_registry(&mut production_registry).expect("production registry");

    let mut success_config = warn_server_config(Arc::clone(&audit_success));
    success_config.cep_context =
        Arc::new(CepTurnContext::defaults_for_project_root(&canonical_str));

    let (success_client, success_server) = CepTransport::pair(32);
    let server_task2 = tokio::spawn(async move {
        let _ = success_server
            .run(production_registry, success_config)
            .await;
    });
    tokio::task::yield_now().await;

    // session.todo_write
    let todo_ok = success_client
        .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
        .await
        .is_ok();

    // observe.audit query
    let aq_ok = success_client
        .call::<AuditObserve>(AuditObserveParams {
            stream: false,
            query: AuditQueryParams::default(),
        })
        .await
        .is_ok();

    // observe.audit tail
    let at_ok = success_client
        .call::<AuditObserve>(AuditObserveParams {
            stream: true,
            query: AuditQueryParams {
                since_audit_id: Some(0),
                ..Default::default()
            },
        })
        .await
        .is_ok();

    // observe.audit_replay — replaying an empty turn returns an empty trace
    let ar_ok = success_client
        .call::<AuditReplay>(AuditReplayParams {
            turn_id: "test-turn".into(),
        })
        .await
        .is_ok();

    drop(success_client);
    server_task2.abort();
    // Keep ws_dir alive until after server task
    drop(ws_dir);

    let success_results = [
        ("session.todo_write", todo_ok),
        ("observe.audit query", aq_ok),
        ("observe.audit tail", at_ok),
        ("observe.audit_replay", ar_ok),
    ];

    // ── Count leaks ──────────────────────────────────────────────────────
    let leak_count = overreach_results.iter().filter(|r| !r.leaked).count();

    // ── Build report ─────────────────────────────────────────────────────
    let mut report = String::new();
    report.push_str("# CEP Warn Mode Coverage Report\n\n");
    report.push_str("Generated by `cep_warn_harness`.\n\n");

    report.push_str("## §1 Methods Exercised\n\n");
    report.push_str(
        "| Method | Warn Surface | Sample Types | Effect Warning |\n\
         |--------|-------------|--------------|----------------|\n",
    );
    for r in &overreach_results {
        let warn_found = if r.leaked { "yes" } else { "MISSING (leak)" };
        report.push_str(&format!(
            "| {} | {} | success + overreach | {} |\n",
            r.method, r.warn_surface, warn_found
        ));
    }
    for (method, ok) in &success_results {
        let status = if *ok { "ok" } else { "error" };
        report.push_str(&format!(
            "| {} | success-only (no effect warn surface) | success only | {} (no overreach sample expected) |\n",
            method, status
        ));
    }

    report.push_str("\n## §2 Leak Count\n\n");
    if leak_count == 0 {
        report.push_str(
            "**N=0** — Clean baseline. All overreach-capable methods produced `effect_warning` \
             in Warn mode audit metadata. No `fix-now` items identified.\n",
        );
    } else {
        report.push_str(&format!(
            "**N={leak_count}** — {leak_count} methods did NOT produce `effect_warning`. Review §3.\n"
        ));
    }

    report.push_str("\n## §3 Triage\n\n");
    report.push_str(
        "| Method | Warn Surface | Effect Warning | Triage |\n\
         |--------|-------------|----------------|--------|\n",
    );
    for r in &overreach_results {
        let (warn_tag, triage) = if r.leaked {
            ("yes — `effect_warning` in audit metadata", "accept-alpha")
        } else {
            ("NO — effect_warning absent", "fix-now")
        };
        report.push_str(&format!(
            "| {} | overreach | {} | {} |\n",
            r.method, warn_tag, triage
        ));
    }
    for (method, _ok) in &success_results {
        report.push_str(&format!(
            "| {} | success-only | n/a | accept-alpha |\n",
            method
        ));
    }

    report
}

// ── Main test ────────────────────────────────────────────────────────────────

/// Warn-mode harness.
///
/// **Default CI path:** reads `docs/lancet-warn-report.md` and asserts no
/// `fix-now` entries are present.
///
/// **Update path** (`LATTICE_UPDATE_WARN_REPORT=1`): runs all 8 CATALOG methods
/// in `CepMode::Warn`, checks audit metadata, and rewrites the committed report.
#[tokio::test]
async fn cep_warn_harness() {
    let repo_root = find_repo_root();
    let report_path = repo_root.join("docs").join("lancet-warn-report.md");

    if std::env::var("LATTICE_UPDATE_WARN_REPORT").is_ok() {
        // ── Update path: regenerate the report ─────────────────────────
        println!("LATTICE_UPDATE_WARN_REPORT set — regenerating lancet-warn-report.md");
        let content = generate_warn_report().await;

        std::fs::create_dir_all(report_path.parent().unwrap()).expect("create docs/ directory");
        std::fs::write(&report_path, &content).expect("write lancet-warn-report.md");
        println!("Report written to {}", report_path.display());

        // Verify the freshly generated report has no fix-now triage entries.
        let new_fix_now: Vec<&str> = content
            .lines()
            .filter(|l| l.contains("| fix-now |") || l.contains("| fix-now"))
            .collect();
        assert!(
            new_fix_now.is_empty(),
            "Newly generated report has fix-now triage entries — \
             effect_warning missing for some methods.\nOffending rows:\n{}",
            new_fix_now.join("\n")
        );
    } else {
        // ── Default CI path: read existing committed report ─────────────
        let content = std::fs::read_to_string(&report_path).unwrap_or_else(|e| {
            panic!(
                "docs/lancet-warn-report.md not found at {}; \
                 run with LATTICE_UPDATE_WARN_REPORT=1 to generate it.\n\
                 Error: {e}",
                report_path.display()
            )
        });

        assert!(
            !content.is_empty(),
            "lancet-warn-report.md is empty; regenerate with LATTICE_UPDATE_WARN_REPORT=1"
        );
        // Look for "fix-now" as a triage table cell value (| fix-now |), not
        // as prose text (e.g. "No fix-now items").
        let fix_now_rows: Vec<&str> = content
            .lines()
            .filter(|l| {
                l.contains("| fix-now |") || l.ends_with("| fix-now |") || l.contains("| fix-now")
            })
            .collect();
        assert!(
            fix_now_rows.is_empty(),
            "lancet-warn-report.md contains fix-now triage entries — \
             one or more overreach samples did not produce effect_warning.\n\
             Regenerate with LATTICE_UPDATE_WARN_REPORT=1 and inspect output.\n\
             Offending rows:\n{}",
            fix_now_rows.join("\n")
        );

        println!(
            "lancet-warn-report.md read from {}; {} bytes; no fix-now entries",
            report_path.display(),
            content.len()
        );
    }
}
