// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Real subprocess/SIGKILL coverage for Rewind action-to-audit windows.

mod support;

use std::time::{Duration, Instant};

use lattice_system_harness::{
    LatticeCommand, SessionSendDriver, SseScript, Termination, TurnEventLine,
};
use serde_json::Value;
use support::{assert_clean_send, direct_world, driver, sequence, tool_call, wire_logged_fake_llm};

#[test]
fn rewind_action_to_audit_windows_survive_sigkill() {
    for barrier in [
        "rewind_workspace_applied",
        "rewind_session_moved",
        "rewind_audit_appended",
    ] {
        run_case(barrier);
    }
}

fn run_case(barrier: &str) {
    let driver = driver();
    let (world, spec) = direct_world(
        &format!("direct-crash-{barrier}"),
        "dl-rewind-crash",
        "[\"mutate.create\"]",
        "",
    );
    let outside = tempfile::tempdir().expect("external crash-surviving counters");
    let provider_log = outside.path().join("provider.log");
    let effect_log = outside.path().join("effects.log");
    world.set_env(
        "LATTICE_TEST_TOOL_EFFECT_LOG",
        effect_log.display().to_string(),
    );
    let server = wire_logged_fake_llm(
        &world,
        "dl-rewind-crash",
        sequence(vec![
            tool_call(
                "create-a",
                "mutate.create",
                r#"{"path":"a.txt","content":"a\n","create_parents":false}"#,
            ),
            SseScript::plain("created a"),
            tool_call(
                "create-b",
                "mutate.create",
                r#"{"path":"b.txt","content":"b\n","create_parents":false}"#,
            ),
            SseScript::plain("created b"),
        ]),
        &provider_log,
    );
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-rewind-crash", "main").expect("session");
    let first = send.send("create a").expect("first Turn");
    assert_clean_send(&first).expect("first Turn clean");
    let second = send.send("create b").expect("second Turn");
    assert_clean_send(&second).expect("second Turn clean");
    let target = committed_id(&first);
    let head = committed_id(&second);
    assert_eq!(server.captured_requests().len(), 4);
    assert_eq!(fsynced_count(&provider_log), 4);
    assert_eq!(fsynced_count(&effect_log), 2);
    assert!(world.project_dir().join("b.txt").is_file());

    let ack = outside.path().join(format!("{barrier}.json"));
    world.set_env("LATTICE_TEST_REWIND_BARRIER", barrier);
    world.set_env("LATTICE_TEST_REWIND_BARRIER_ACK", ack.display().to_string());
    let command = LatticeCommand {
        args: vec![
            "--json".into(),
            "session".into(),
            "rewind".into(),
            send.session_id().into(),
            target.clone(),
        ],
        timeout: Some(Duration::from_secs(30)),
    };
    let running = driver.start(&world, &command).expect("spawn rewind");
    let facts = wait_for_ack(&ack, Duration::from_secs(20));
    assert_eq!(facts["barrier"], barrier);
    assert!(facts["facts"]["workspace_history_oid"].as_str().is_some());
    let audit_len = facts["facts"]["audit_records_on_chain"]
        .as_u64()
        .expect("audit length");
    match barrier {
        "rewind_workspace_applied" => {
            assert_eq!(facts["facts"]["session_head_commit_id"], head);
            assert_eq!(audit_len, 0);
        }
        "rewind_session_moved" => {
            assert_eq!(facts["facts"]["session_head_commit_id"], target);
            assert_eq!(audit_len, 0);
        }
        "rewind_audit_appended" => {
            assert_eq!(facts["facts"]["session_head_commit_id"], target);
            assert_eq!(audit_len, 1);
        }
        _ => unreachable!(),
    }
    assert_eq!(
        running.kill_and_wait().expect("kill rewind").termination,
        Termination::Killed
    );
    assert_eq!(server.captured_requests().len(), 4);
    assert_eq!(fsynced_count(&provider_log), 4);
    assert_eq!(fsynced_count(&effect_log), 2);
    assert!(!world.project_dir().join("b.txt").exists());
    world.unset_env("LATTICE_TEST_REWIND_BARRIER");
    world.unset_env("LATTICE_TEST_REWIND_BARRIER_ACK");

    if barrier != "rewind_audit_appended" {
        let completed = driver.run(&world, &command).expect("retry rewind");
        assert!(
            completed.status.success(),
            "retry after {barrier} failed: {}",
            String::from_utf8_lossy(&completed.stderr)
        );
    }
    assert_eq!(server.captured_requests().len(), 4);
    assert_eq!(fsynced_count(&provider_log), 4);
    assert_eq!(fsynced_count(&effect_log), 2);
}

fn committed_id(outcome: &lattice_system_harness::SendExecution) -> String {
    outcome
        .events
        .iter()
        .find_map(|event| match event {
            TurnEventLine::TurnCommitted { commit_id, .. } => Some(commit_id.clone()),
            _ => None,
        })
        .expect("TurnCommitted")
}

fn fsynced_count(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|content| content.lines().count())
        .unwrap_or(0)
}

fn wait_for_ack(path: &std::path::Path, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        if path.is_file() {
            return serde_json::from_slice(&std::fs::read(path).expect("read barrier ack"))
                .expect("barrier ack JSON");
        }
        assert!(Instant::now() < deadline, "barrier ack was not published");
        std::thread::sleep(Duration::from_millis(5));
    }
}
