// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 缺库拒绝与无调用项目的空状态；不伪造账本 OID。
mod support;
use lattice_system_harness::{SessionSendDriver, SseScript};
use serde_json::Value;
use support::{cli, direct_world, driver, wire_fake_llm};
#[test]
fn lancet_status_missing_database_fails_without_creating_it() {
    let (world, spec) = direct_world("lancet-status-missing", "plain", "[]", "");
    let out = cli(&world, &spec, &["--json", "lancet", "status"]);
    assert!(out.is_clean_exit(1), "{}", out.summary());
    assert!(out.stderr.is_empty());
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["kind"], "store_unavailable");
    assert!(!world.session_store_dir().join("lattice.redb").exists());
}
#[test]
fn lancet_status_no_calls_reports_empty_state_json_and_text() {
    let (world, spec) = direct_world("lancet-status-empty", "plain", "[]", "");
    let server = wire_fake_llm(&world, "plain", SseScript::plain("no calls"));
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "plain");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let turn = send.send("text only").unwrap();
        evidence.record_command("seed", &turn.command);
        support::assert_clean_send(&turn).unwrap();
        let out = cli(&world, &spec, &["--json", "lancet", "status"]);
        evidence.record_command("status JSON", &out);
        assert!(out.is_clean_exit(0));
        let status: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(status["latest_change"], 0);
        assert_eq!(status["recent"], serde_json::json!([]));
        let out = cli(&world, &spec, &["lancet", "status"]);
        evidence.record_command("status text", &out);
        assert!(out.is_clean_exit(0));
        assert_eq!(
            std::str::from_utf8(&out.stdout).unwrap(),
            "lancet status\nlatest_change=0\n"
        );
        assert_eq!(server.captured_requests().len(), 1);
    }));
    evidence.record_captures(server.captured_requests());
    evidence
        .finish_report(result.err().map(|p| format!("{p:?}")))
        .unwrap();
}
