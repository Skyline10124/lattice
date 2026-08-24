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
//! Runs the remaining native catalog method(s) through `CepMode::Warn` and
//! verifies that an intentional effect-region overreach remains non-blocking.
//! writes a fresh `docs/lancet-warn-report.md` at the repo root.

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
    CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::policy::CepMode;
use tempfile::TempDir;

fn warn_server_config() -> ServerRunConfig {
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.cep_mode = CepMode::Warn;
    config
}

fn test_binding(method: &str, params: &serde_json::Value) -> lancet_protocol::InvocationBinding {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(100);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    lancet_protocol::InvocationBinding {
        invocation_id: lancet_protocol::InvocationId::parse(&format!(
            "00000000-0000-4000-8000-{ordinal:012x}"
        ))
        .expect("invocation id"),
        identity: lancet_protocol::BindingIdentity {
            session_id: lancet_protocol::SessionId::parse("00000000000000000000000000000001")
                .expect("session id"),
            turn_id: lancet_protocol::TurnId::parse("00000000-0000-4000-8000-000000000001")
                .expect("turn id"),
            model_step_ordinal: ordinal as u32,
            part_ordinal: 0,
            method: method.to_string(),
            descriptor_identity: "warn-harness".to_string(),
            arguments_digest: lancet_protocol::arguments_digest(params),
        },
    }
}

fn reserve(config: &ServerRunConfig, binding: &lancet_protocol::InvocationBinding) {
    config
        .invocations
        .reserve_batch(
            lancet_protocol::PlanKey {
                turn_id: binding.identity.turn_id,
                model_step_ordinal: binding.identity.model_step_ordinal,
            },
            std::slice::from_ref(binding),
        )
        .expect("reserve invocation");
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
    let config = warn_server_config();
    let config_for_call = config.clone();
    let (client, server) = CepTransport::pair(8);
    let server_task =
        tokio::spawn(async move { server.run_native(registry, config).await.unwrap() });

    let params_value = serde_json::to_value(&params).expect("serialize params");
    let binding = test_binding(M::NAME, &params_value);
    reserve(&config_for_call, &binding);

    let leaked = client.call::<M>(binding, params).await.is_ok();
    drop(client);
    server_task.abort();
    OverreachResult {
        method: method_name,
        warn_surface: "overreach",
        leaked,
    }
}

fn find_repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate manifest lives two levels below the workspace root")
        .to_path_buf()
}

async fn generate_warn_report() -> String {
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

    let ws_dir = TempDir::new().expect("temp dir");
    let ws_root = ws_dir.path();
    std::fs::create_dir_all(ws_root.join("src")).unwrap();
    std::fs::write(ws_root.join("src/lib.rs"), "fn main() {}").unwrap();
    let canonical_root = std::fs::canonicalize(ws_root).unwrap_or_else(|_| ws_root.to_path_buf());
    let canonical_str = canonical_root.to_str().unwrap().to_string();

    let mut production_registry = MethodRegistry::new();
    register_production_method_registry(&mut production_registry).expect("production registry");

    let mut success_config = warn_server_config();
    success_config.cep_context =
        Arc::new(CepTurnContext::defaults_for_project_root(&canonical_str));
    let success_config_for_call = success_config.clone();

    let (success_client, success_server) = CepTransport::pair(32);
    let server_task2 = tokio::spawn(async move {
        let _ = success_server
            .run_native(production_registry, success_config)
            .await;
    });
    tokio::task::yield_now().await;

    let fetch_params = FetchParams {
        url: "https://example.com/".into(),
        headers: Default::default(),
    };
    let params_value = serde_json::to_value(&fetch_params).expect("serialize fetch params");
    let binding = test_binding(Fetch::NAME, &params_value);
    reserve(&success_config_for_call, &binding);
    let native_ok = success_client
        .call::<Fetch>(binding, fetch_params)
        .await
        .is_ok();

    drop(success_client);
    server_task2.abort();
    drop(ws_dir);

    let success_results = [("fetch.web", native_ok)];
    let leak_count = overreach_results.iter().filter(|r| !r.leaked).count();

    let mut report = String::new();
    report.push_str("# CEP Warn Mode Coverage Report\n\n");
    report.push_str("Generated by `cep_warn_harness`.\n\n");

    report.push_str("## §1 Methods Exercised\n\n");
    report.push_str(
        "| Method | Warn Surface | Sample Types | Warn Allowed Overreach |\n\
         |--------|-------------|--------------|------------------------|\n",
    );
    for r in &overreach_results {
        let warn_found = if r.leaked { "yes" } else { "MISSING (blocked)" };
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
            "**N=0** — Clean baseline. Every intentional overreach remained non-blocking in Warn \
             mode. No `fix-now` items identified.\n",
        );
    } else {
        report.push_str(&format!(
            "**N={leak_count}** — {leak_count} intentional overreach samples were blocked in Warn. Review §3.\n"
        ));
    }

    report.push_str("\n## §3 Triage\n\n");
    report.push_str(
        "| Method | Warn Surface | Warn Allowed Overreach | Triage |\n\
         |--------|-------------|------------------------|--------|\n",
    );
    for r in &overreach_results {
        let (warn_tag, triage) = if r.leaked {
            ("yes", "accept-alpha")
        } else {
            ("NO — blocked", "fix-now")
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

#[tokio::test]
async fn cep_warn_harness() {
    let repo_root = find_repo_root();
    let report_path = repo_root.join("docs").join("lancet-warn-report.md");

    if std::env::var("LATTICE_UPDATE_WARN_REPORT").is_ok() {
        println!("LATTICE_UPDATE_WARN_REPORT set — regenerating lancet-warn-report.md");
        let content = generate_warn_report().await;

        std::fs::create_dir_all(report_path.parent().unwrap()).expect("create docs/ directory");
        std::fs::write(&report_path, &content).expect("write lancet-warn-report.md");
        println!("Report written to {}", report_path.display());

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
