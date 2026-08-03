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
//! Runs the remaining native CATALOG method(s) through `CepMode::Warn`, collects
//! `effect_warning` from the `CepAuditStore`, and
//! writes a fresh `docs/lancet-warn-report.md` at the repo root.
//!
//! Method categories
//! - **Overreach-capable:** use a custom handler that deliberately sets an
//!   empty `AuthorizedEffectRegion` and records an out-of-scope effect so
//!   `effect_warning` appears in the audit metadata.
//! - **Success-only:** `session.todo_write` — no meaningful effect-region
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
use lancet_catalog::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_catalog::{CatalogMethod, CepMethod};
use lancet_native::{
    CepAuditStore, CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
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

// `overreach_registry_observe_workspace` went with the native handler (#30).
// A warn-mode overreach fixture registers a *native* handler that records an
// effect outside its authorized region; `observe.workspace` is a Component Tool
// now, and a Component's effects are recorded by the host through its Cell
// evidence, never by a handler this harness could register. The equivalent
// negative case for the Component path is
// `lancet_runtime::host_workspace::tests::an_unadmitted_cell_is_refused_before_the_filesystem`.

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
    let server_task =
        tokio::spawn(async move { server.run_native(registry, config).await.unwrap() });

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

/// The workspace root of *this* source tree: `crates/lancet-native` -> `crates`
/// -> repo root.
///
/// `CARGO_MANIFEST_DIR` is baked in at compile time, so it always names the
/// tree this test binary was built from. Do not search upward for a marker
/// directory: the previous implementation looked for `.planning/`, which is
/// gitignored (`.gitignore`) and force-add-blocked (`.githooks/pre-commit`).
/// It therefore never exists in CI or a fresh clone — and when the same
/// repository is checked out as a `git worktree` under `.claude/worktrees/`,
/// the upward walk escaped the worktree and resolved to the *main* checkout,
/// so the harness read (and with `LATTICE_UPDATE_WARN_REPORT=1` wrote) another
/// checkout's `docs/lancet-warn-report.md`. See GitHub #79.
fn find_repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate manifest lives two levels below the workspace root")
        .to_path_buf()
}

// ── Report generator ─────────────────────────────────────────────────────────

async fn generate_warn_report() -> String {
    // ── Overreach-capable canonical methods ──────────────────────────────

    let overreach_results = vec![
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

    // Single production server for all success-only methods.
    let audit_success = Arc::new(CepAuditStore::in_memory().expect("audit"));

    let mut production_registry = MethodRegistry::new();
    register_production_method_registry(&mut production_registry).expect("production registry");

    let mut success_config = warn_server_config(Arc::clone(&audit_success));
    success_config.cep_context =
        Arc::new(CepTurnContext::defaults_for_project_root(&canonical_str));

    let (success_client, success_server) = CepTransport::pair(32);
    let server_task2 = tokio::spawn(async move {
        let _ = success_server
            .run_native(production_registry, success_config)
            .await;
    });
    tokio::task::yield_now().await;

    // fetch.web: the remaining native method after #31. This leg needs a
    // *native* method, because it exercises the native policy lifecycle in warn
    // mode; a Component Tool is governed by Cell admission instead and would
    // prove nothing here. It has moved thrice for that reason — off
    // `session.todo_write` in #29, `observe.workspace` in #30, and
    // `mutate.create` in #31, as each became a Component. The call may fail on
    // network policy; we only care that it was routed as a native method.
    let native_ok = success_client
        .call::<Fetch>(FetchParams {
            url: "https://example.com/".into(),
            headers: Default::default(),
        })
        .await
        .is_ok();

    drop(success_client);
    server_task2.abort();
    // Keep ws_dir alive until after server task
    drop(ws_dir);

    let success_results = [("fetch.web", native_ok)];

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
/// **Update path** (`LATTICE_UPDATE_WARN_REPORT=1`): runs the remaining native CATALOG method(s)
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
