//! `lattice lancet` — CEP inspection and self-check subcommands (CLI3-01, CLI3-03).
//!
//! - `lancet status`: reads `.lattice/cep_audit.db` directly (no CEP server) and shows
//!   the latest envelope.declare fields, recent calls, and last guard verdict.
//! - `lancet doctor`: catalog count, transport smoke, invariant-card parse, and an
//!   isolated `submit_intent` smoke that asserts `unknowns == 0`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;
use colored::Colorize;
use lancet_executor::invariants::loader::load_cards_from_dir;
use lancet_executor::invariants::resolve_invariants_dir;
use lancet_executor::{
    register_production_method_registry, AuditQueryFilters, CepAuditStore, CepTurnContext,
    LancetTransport, MethodRegistry, ServerRunConfig,
};
use lancet_protocol::catalog::catalog;
use lancet_protocol::envelope::BaseExecutionEnvelope;
use lancet_protocol::methods::envelope::{SubmitIntent, SubmitIntentParams};

/// `lattice lancet` subcommand group.
#[derive(Subcommand)]
pub enum LancetAction {
    /// Show audit-derived CEP status: active envelope, recent calls, last guard verdict.
    Status {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Run CEP self-check: catalog count, transport, invariant cards, submit_intent smoke.
    Doctor,
}

/// Dispatch the `lancet` subcommand group. `None` action defaults to `Status`.
pub fn run(action: Option<LancetAction>, global_json: bool) -> Result<()> {
    match action.unwrap_or(LancetAction::Status { json: false }) {
        LancetAction::Status { json } => run_status(json || global_json),
        LancetAction::Doctor => run_doctor(global_json),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn audit_db_path(project_root: &Path) -> PathBuf {
    project_root.join(".lattice").join("cep_audit.db")
}

fn compute_duration_ms(started_at: &str, ended_at: Option<&str>) -> Option<i64> {
    let ended = ended_at?;
    let start = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(ended).ok()?;
    Some(end.signed_duration_since(start).num_milliseconds())
}

// ---------------------------------------------------------------------------
// lancet status
// ---------------------------------------------------------------------------

pub(crate) struct LancetStatusOutput {
    pub active_envelope: Option<serde_json::Value>,
    pub no_active_envelope_observed: bool,
    pub recent_calls: Vec<RecentCallEntry>,
    pub last_guard_verdict: Option<String>,
}

pub(crate) struct RecentCallEntry {
    pub audit_id: i64,
    pub method: String,
    pub impact: String,
    pub verdict: Option<String>,
    pub duration_ms: Option<i64>,
}

/// Build status data from the audit store without spawning a CEP server.
pub(crate) fn collect_status(project_root: &Path) -> Result<LancetStatusOutput> {
    let db_path = audit_db_path(project_root);

    if !db_path.exists() {
        return Ok(LancetStatusOutput {
            active_envelope: None,
            no_active_envelope_observed: true,
            recent_calls: vec![],
            last_guard_verdict: None,
        });
    }

    let store =
        CepAuditStore::open(&db_path).map_err(|e| anyhow::anyhow!("open audit store: {e}"))?;

    // Latest envelope.declare row — raw metadata_json only (D-02 review decision).
    let declare_rows = store
        .query_rows(&AuditQueryFilters {
            method: Some("envelope.declare".to_string()),
            ..Default::default()
        })
        .map_err(|e| anyhow::anyhow!("query envelope.declare rows: {e}"))?;

    let active_envelope = declare_rows.last().and_then(|row| {
        row.metadata_json
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
    });
    let no_active_envelope_observed = active_envelope.is_none();

    // All rows for recent-calls window and last guard verdict.
    let all_rows = store
        .query_rows(&AuditQueryFilters::default())
        .map_err(|e| anyhow::anyhow!("query all rows: {e}"))?;

    let last_guard_verdict = all_rows.last().map(|r| {
        r.semantic_status
            .clone()
            .unwrap_or_else(|| r.guard_stage.clone())
    });

    let recent_start = all_rows.len().saturating_sub(20);
    let recent_calls = all_rows[recent_start..]
        .iter()
        .rev()
        .map(|row| RecentCallEntry {
            audit_id: row.audit_id,
            method: row.method.clone(),
            impact: row.impact_kind.clone(),
            verdict: row.semantic_status.clone(),
            duration_ms: compute_duration_ms(row.started_at.as_str(), row.ended_at.as_deref()),
        })
        .collect();

    Ok(LancetStatusOutput {
        active_envelope,
        no_active_envelope_observed,
        recent_calls,
        last_guard_verdict,
    })
}

fn run_status(json: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    let output = collect_status(&project_root)?;

    if json {
        let value = serde_json::json!({
            "active_envelope": output.active_envelope,
            "no_active_envelope_observed": output.no_active_envelope_observed,
            "recent_calls": output.recent_calls.iter().map(|c| serde_json::json!({
                "audit_id": c.audit_id,
                "method": c.method,
                "impact": c.impact,
                "verdict": c.verdict,
                "duration_ms": c.duration_ms,
            })).collect::<Vec<_>>(),
            "last_guard_verdict": output.last_guard_verdict,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        render_status_text(&output);
    }
    Ok(())
}

fn render_status_text(output: &LancetStatusOutput) {
    println!("{}", "lancet status".dimmed());
    match &output.active_envelope {
        Some(env) => {
            println!("{}: {}", "Active Envelope".bold(), "found".green());
            for key in &["intent", "capabilities", "scope", "impact"] {
                if let Some(val) = env.get(*key) {
                    println!("  {} {}", format!("{key}:").green(), val);
                }
            }
        }
        None => println!(
            "{}: {}",
            "Active Envelope".bold(),
            "no active envelope observed".yellow()
        ),
    }
    println!("\n{}", "Recent CEP Calls:".bold());
    if output.recent_calls.is_empty() {
        println!("  {}", "none".dimmed());
    } else {
        for call in &output.recent_calls {
            let verdict = call.verdict.as_deref().unwrap_or("?");
            let dur = call
                .duration_ms
                .map(|d| format!("{}ms", d))
                .unwrap_or_default();
            println!(
                "  [{}] {} {} {} {}",
                call.audit_id,
                call.method.cyan(),
                call.impact.dimmed(),
                verdict,
                dur
            );
        }
    }
    let verdict_str = output.last_guard_verdict.as_deref().unwrap_or("none");
    println!(
        "\n{}: {}",
        "Last Guard Verdict".bold(),
        verdict_str.dimmed()
    );
}

// ---------------------------------------------------------------------------
// lancet doctor
// ---------------------------------------------------------------------------

/// Public entry point — derives invariants dir from `$PWD/.lancet/invariants/`.
pub fn run_doctor(json: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    let invariants_dir = resolve_invariants_dir(&project_root, None);
    run_doctor_inner(&invariants_dir, json)
}

/// Testable core — accepts an explicit invariants directory.
pub(crate) fn run_doctor_inner(invariants_dir: &Path, json: bool) -> Result<()> {
    let mut ok_items: Vec<String> = Vec::new();
    let mut issues: Vec<String> = Vec::new();

    // 1. CATALOG method count (must be exactly 14).
    let catalog_len = catalog().len();
    if catalog_len == 14 {
        ok_items.push(format!("catalog: {catalog_len} methods (expected 14)"));
    } else {
        issues.push(format!("catalog: expected 14 methods, got {catalog_len}"));
    }

    // 2. Production method registry registers without error.
    {
        let mut registry = MethodRegistry::new();
        match register_production_method_registry(&mut registry) {
            Ok(()) => ok_items.push("registry: production methods registered".to_string()),
            Err(e) => issues.push(format!("registry: {}", e.message)),
        }
    }

    // 3. Invariant cards — parse errors fail doctor (D-12).
    let card_ids: Vec<String> = match load_cards_from_dir(invariants_dir) {
        Ok(cards) if cards.is_empty() => {
            ok_items.push("invariants: ok_empty (no cards)".to_string());
            vec![]
        }
        Ok(cards) => {
            let ids: Vec<String> = cards.iter().map(|c| c.id.clone()).collect();
            ok_items.push(format!(
                "invariants: ok_loaded ({} card(s): {})",
                ids.len(),
                ids.join(", ")
            ));
            ids
        }
        Err(e) => {
            issues.push(format!("invariants: parse error: {e}"));
            // Fail fast — remaining checks depend on invariant integrity.
            return emit_doctor_result(&ok_items, &issues, &[], json);
        }
    };

    // 4. submit_intent smoke in isolated temp workspace (D-03).
    match run_submit_intent_smoke() {
        Ok(()) => ok_items.push("submit_intent smoke: ok (unknowns == 0)".to_string()),
        Err(e) => issues.push(format!("submit_intent smoke: {e}")),
    }

    emit_doctor_result(&ok_items, &issues, &card_ids, json)
}

fn emit_doctor_result(
    ok_items: &[String],
    issues: &[String],
    card_ids: &[String],
    json: bool,
) -> Result<()> {
    let all_ok = issues.is_empty();

    if json {
        let checks: Vec<_> = ok_items
            .iter()
            .map(|s| serde_json::json!({"status": "ok", "message": s}))
            .chain(
                issues
                    .iter()
                    .map(|s| serde_json::json!({"status": "fail", "message": s})),
            )
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": all_ok,
                "checks": checks,
                "card_ids": card_ids,
            }))?
        );
    } else {
        println!("{}", "lancet doctor".dimmed());
        for item in ok_items {
            println!("  {} {}", "\u{2713}".green(), item);
        }
        for issue in issues {
            println!("  {} {}", "\u{2717}".red(), issue.red());
        }
    }

    if !all_ok {
        anyhow::bail!("lancet doctor: {} check(s) failed", issues.len());
    }
    Ok(())
}

/// Run an isolated submit_intent smoke using an in-memory CEP runtime (D-03).
///
/// Uses the current directory as the project root so that `canonicalize` succeeds.
/// The submit is for a "read" action on `Cargo.toml` — a file that exists in every
/// workspace/crate directory. Asserts `impact.unknowns` is empty.
///
/// Handles two call contexts:
/// - From a synchronous caller without a tokio runtime (tests, benchmarks): creates a
///   fresh `current_thread` runtime.
/// - From within `#[tokio::main]` (the production CLI path): uses `block_in_place` +
///   `Handle::current().block_on()` to avoid panicking with "nested runtime" errors.
pub(crate) fn run_submit_intent_smoke() -> Result<()> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // Inside an existing tokio runtime — block_in_place prevents nesting panic.
            tokio::task::block_in_place(|| handle.block_on(submit_intent_smoke_async()))
        }
        Err(_) => {
            // No runtime active — create a lightweight current-thread runtime.
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("tokio runtime for submit_intent smoke")?
                .block_on(submit_intent_smoke_async())
        }
    }
}

async fn submit_intent_smoke_async() -> Result<()> {
    let project_root = std::env::current_dir().context("cwd for smoke")?;
    let root_str = project_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("project root path is not UTF-8"))?;

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry)
        .map_err(|e| anyhow::anyhow!("register CEP methods: {}", e.message))?;

    // submit_intent requires MutateSessionState capability to store the derived envelope.
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.base_envelope =
        BaseExecutionEnvelope::for_workspace_root(root_str, "lancet doctor smoke")
            .with_session_state();
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root_str));

    let (client, server) = LancetTransport::pair(64);
    let server_task = tokio::spawn(async move {
        server
            .run(registry, config)
            .await
            .map_err(|e| format!("CEP server: {}", e.message))
            .expect("CEP server");
    });

    let result = client
        .call::<SubmitIntent>(SubmitIntentParams {
            action: "read".to_string(),
            target: "Cargo.toml".to_string(),
            params: None,
            supersede: false,
            reason: None,
        })
        .await
        .map_err(|e| anyhow::anyhow!("submit_intent smoke failed: {}", e.message))?;

    server_task.abort();

    if !result.impact.unknowns.is_empty() {
        anyhow::bail!(
            "submit_intent smoke: {} unknown(s) in impact analysis",
            result.impact.unknowns.len()
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use lancet_executor::audit::{AuditOutcome, CallAuditMetadata};
    use lancet_executor::{CepAuditContext, CepAuditStore};
    use tempfile::TempDir;

    // -- Clap surface tests --

    /// Test wrapper that wraps LancetAction as a top-level subcommand.
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: LancetAction,
    }

    fn try_parse_action(args: &[&str]) -> std::result::Result<LancetAction, clap::Error> {
        let mut full = vec!["lancet"];
        full.extend_from_slice(args);
        TestCli::try_parse_from(full).map(|c| c.action)
    }

    #[test]
    fn lancet_status_json_flag_parses() {
        let action = try_parse_action(&["status", "--json"]);
        assert!(action.is_ok(), "should parse `lancet status --json`");
        match action.unwrap() {
            LancetAction::Status { json } => assert!(json),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn lancet_status_no_args_parses() {
        let action = try_parse_action(&["status"]);
        assert!(action.is_ok());
        match action.unwrap() {
            LancetAction::Status { json } => assert!(!json),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn lancet_doctor_parses() {
        let action = try_parse_action(&["doctor"]);
        assert!(action.is_ok());
        assert!(matches!(action.unwrap(), LancetAction::Doctor));
    }

    // -- Status tests --

    fn seed_audit_db(dir: &Path) {
        let db_path = dir.join(".lattice").join("cep_audit.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let audit = CepAuditStore::open(&db_path).expect("open audit db");
        let ctx = CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            model_id: "test-model".into(),
            runtime_version: "test".into(),
        };
        // envelope.declare row with intent/capabilities in metadata_json (D-02)
        let start = audit
            .start_call(
                &ctx,
                1,
                "envelope.declare",
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "declared".into(),
                },
                Some("native_handler"),
                Some(r#"{"intent":"smoke test","capabilities":["read"],"scope":"."}"#),
            )
            .unwrap();
        // A regular call row
        let start2 = audit
            .start_call(
                &ctx,
                2,
                "workspace.read_file",
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start2,
                AuditOutcome::Success {
                    summary: "read ok".into(),
                },
                Some("native_handler"),
                None,
            )
            .unwrap();
    }

    #[test]
    fn lancet_status_missing_db_returns_empty_state() {
        let dir = TempDir::new().unwrap();
        let output = collect_status(dir.path()).unwrap();
        assert!(output.no_active_envelope_observed);
        assert!(output.active_envelope.is_none());
        assert!(output.recent_calls.is_empty());
        assert!(output.last_guard_verdict.is_none());
    }

    #[test]
    fn lancet_status_seeded_audit_json() {
        let dir = TempDir::new().unwrap();
        seed_audit_db(dir.path());

        let output = collect_status(dir.path()).unwrap();

        // Should have parsed the declare row's metadata_json
        assert!(
            output.active_envelope.is_some() || !output.no_active_envelope_observed,
            "should observe envelope from declare row"
        );
        // Should have at least the two seeded rows
        assert!(!output.recent_calls.is_empty(), "should have recent calls");
        // Last guard verdict derived from newest row
        assert!(
            output.last_guard_verdict.is_some(),
            "should have last guard verdict"
        );
        // Check JSON schema keys
        let json = serde_json::json!({
            "active_envelope": output.active_envelope,
            "no_active_envelope_observed": output.no_active_envelope_observed,
            "recent_calls": output.recent_calls.iter().map(|c| serde_json::json!({
                "audit_id": c.audit_id,
                "method": c.method,
                "impact": c.impact,
                "verdict": c.verdict,
                "duration_ms": c.duration_ms,
            })).collect::<Vec<_>>(),
            "last_guard_verdict": output.last_guard_verdict,
        });
        assert!(
            json.get("active_envelope").is_some(),
            "JSON must have active_envelope"
        );
        assert!(
            json.get("no_active_envelope_observed").is_some(),
            "JSON must have no_active_envelope_observed"
        );
        assert!(
            json.get("recent_calls").is_some(),
            "JSON must have recent_calls"
        );
        assert!(
            json.get("last_guard_verdict").is_some(),
            "JSON must have last_guard_verdict"
        );

        // Verify method values in recent_calls
        let calls = json["recent_calls"].as_array().unwrap();
        let has_declare = calls
            .iter()
            .any(|c| c["method"].as_str() == Some("envelope.declare"));
        assert!(
            has_declare,
            "recent_calls should include envelope.declare row"
        );
    }

    // -- Doctor tests --

    #[test]
    fn lancet_doctor_passes_on_repo_root() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");
        let invariants_dir = workspace_root.join(".lancet").join("invariants");

        let prev = std::env::current_dir().ok();
        std::env::set_current_dir(workspace_root).unwrap();
        let result = run_doctor_inner(&invariants_dir, false);
        if let Some(p) = prev {
            let _ = std::env::set_current_dir(p);
        }
        assert!(
            result.is_ok(),
            "doctor should pass on repo root: {:?}",
            result
        );
    }

    #[test]
    fn lancet_doctor_reports_invariant_cards() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");
        let invariants_dir = workspace_root.join(".lancet").join("invariants");

        let cards =
            load_cards_from_dir(&invariants_dir).expect("cards should load without parse errors");

        assert!(
            cards.len() >= 2,
            "expected at least 2 invariant cards, got {}",
            cards.len()
        );

        // alpha-wire-freeze card
        let wire_freeze = cards
            .iter()
            .find(|c| c.id == "ALPHA-WIRE-FREEZE")
            .expect("ALPHA-WIRE-FREEZE card must exist");
        assert!(
            wire_freeze.title.contains("wire freeze") || wire_freeze.title.contains("Wire"),
            "wire-freeze title: {}",
            wire_freeze.title
        );
        assert!(
            !wire_freeze.forbidden.is_empty(),
            "wire-freeze card should have forbidden entries"
        );
        assert!(matches!(
            wire_freeze.anchor,
            lancet_executor::invariants::types::CardAnchor::Glob(_)
        ));

        // no-legacy-public-expansion card
        let legacy_card = cards
            .iter()
            .find(|c| c.id == "NO-LEGACY-EXPANSION")
            .expect("NO-LEGACY-EXPANSION card must exist");
        assert!(
            !legacy_card.forbidden.is_empty(),
            "no-legacy-expansion card should have forbidden entries"
        );
        assert!(matches!(
            legacy_card.anchor,
            lancet_executor::invariants::types::CardAnchor::Glob(_)
        ));
    }

    #[test]
    fn lancet_doctor_submit_intent_unknowns_zero() {
        let result = run_submit_intent_smoke();
        assert!(
            result.is_ok(),
            "submit_intent smoke should succeed: {:?}",
            result
        );
    }

    #[test]
    fn lancet_doctor_fails_on_broken_card() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("bad.toml"), "this is not valid toml {{{").unwrap();

        let result = run_doctor_inner(dir.path(), false);
        assert!(
            result.is_err(),
            "doctor should fail when invariants dir has an unparseable card"
        );
    }
}
