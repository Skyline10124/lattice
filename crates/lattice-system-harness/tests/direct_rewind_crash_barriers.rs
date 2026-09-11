// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 通过真实后台SIGSTOP/SIGKILL验证Rewind文件、引用与审计窗口，不改变原故障阶段。

mod support;

use std::time::{Duration, Instant};

use lattice_system_harness::{
    CommandOutcome, LatticeCommand, ScenarioWorld, SessionSendDriver, SseScript,
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
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        let send = SessionSendDriver::new(&driver, &world, &spec, "dl-rewind-crash", "main")
            .expect("session");
        let baseline_daemon = support::crash::start(&world, &spec, send.budget());
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
        evidence.record_command("create a", &first.command);
        evidence.record_command("create b", &second.command);
        assert!(first.command.is_clean_exit(0) && second.command.is_clean_exit(0));
        world.stop_daemon().unwrap();
        let stopped = baseline_daemon.wait(None).unwrap();
        evidence.record_command("stop baseline daemon before arming barrier", &stopped);
        assert!(stopped.is_clean_exit(0));

        let ack = outside.path().join(format!("{barrier}.json"));
        world.set_env("LATTICE_TEST_REWIND_BARRIER", barrier);
        world.set_env("LATTICE_TEST_REWIND_BARRIER_ACK", ack.display().to_string());
        let command = || {
            send.budget().check(&spec.name);
            LatticeCommand {
                args: vec![
                    "--json".into(),
                    "session".into(),
                    "rewind".into(),
                    send.session_id().into(),
                    target.clone(),
                ],
                timeout: Some(send.budget().cap_timeout(spec.step_timeout())),
            }
        };
        let daemon = support::crash::start(&world, &spec, send.budget());
        let running = driver.start(&world, &command()).expect("spawn rewind");
        let facts = wait_for_ack(&ack, send.budget().cap_timeout(Duration::from_secs(20)));
        assert_eq!(facts["barrier"], barrier);
        assert_eq!(facts["facts"]["session_id"], send.session_id());
        assert!(
            facts["facts"]["target_turn_id"]
                .as_str()
                .is_some_and(|id| id.len() == 36),
            "barrier target must expose the typed TurnId's canonical form"
        );
        assert!(facts["facts"]["workspace_history_oid"].as_str().is_some());
        let audit_page = &facts["facts"]["audit_page"];
        assert_eq!(audit_page["has_older"], false);
        let audit_len = audit_page["records"]
            .as_array()
            .expect("audit records")
            .len() as u64;
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
        support::crash::kill(&world, daemon, running, &mut evidence, send.budget());
        assert_eq!(server.captured_requests().len(), 4);
        assert_eq!(fsynced_count(&provider_log), 4);
        assert_eq!(fsynced_count(&effect_log), 2);
        assert!(!world.project_dir().join("b.txt").exists());
        world.unset_env("LATTICE_TEST_REWIND_BARRIER");
        world.unset_env("LATTICE_TEST_REWIND_BARRIER_ACK");

        if barrier != "rewind_audit_appended" {
            let retry_ack = outside.path().join(format!("{barrier}-retry-audit.json"));
            world.set_env("LATTICE_TEST_REWIND_BARRIER", "rewind_audit_appended");
            world.set_env(
                "LATTICE_TEST_REWIND_BARRIER_ACK",
                retry_ack.display().to_string(),
            );
            let daemon = support::crash::start(&world, &spec, send.budget());
            let retry = driver
                .start(&world, &command())
                .expect("spawn retry Rewind");
            let retry_facts = wait_for_ack(
                &retry_ack,
                send.budget().cap_timeout(Duration::from_secs(20)),
            );
            assert_eq!(retry_facts["barrier"], "rewind_audit_appended");
            assert_eq!(retry_facts["facts"]["session_head_commit_id"], target);
            assert_eq!(
                retry_facts["facts"]["audit_page"]["records"]
                    .as_array()
                    .expect("retry audit records")
                    .len(),
                1,
                "retry must append exactly one success record"
            );
            support::crash::kill(&world, daemon, retry, &mut evidence, send.budget());
        }

        world.unset_env("LATTICE_TEST_REWIND_BARRIER");
        world.unset_env("LATTICE_TEST_REWIND_BARRIER_ACK");
        let completed = driver
            .run(&world, &command())
            .expect("complete retry Rewind");
        assert_successful_rewind(&completed, &target, barrier);
        assert_completed_state(&send, &world, &target);
        assert_eq!(server.captured_requests().len(), 4);
        assert_eq!(fsynced_count(&provider_log), 4);
        assert_eq!(fsynced_count(&effect_log), 2);

        // 完成后再次武装追加屏障；精确重试必须在恢复文件、移动引用和追加审计之前返回。
        let final_ack = outside
            .path()
            .join(format!("{barrier}-completed-retry.json"));
        world.set_env("LATTICE_TEST_REWIND_BARRIER", "rewind_audit_appended");
        world.set_env(
            "LATTICE_TEST_REWIND_BARRIER_ACK",
            final_ack.display().to_string(),
        );
        let daemon = support::crash::start(&world, &spec, send.budget());
        let replayed = driver
            .run(&world, &command())
            .expect("replay completed Rewind");
        assert_successful_rewind(&replayed, &target, barrier);
        assert!(
            !final_ack.exists(),
            "completed retry after {barrier} reached the audit-append barrier"
        );
        world.unset_env("LATTICE_TEST_REWIND_BARRIER");
        world.unset_env("LATTICE_TEST_REWIND_BARRIER_ACK");
        assert_completed_state(&send, &world, &target);
        assert_eq!(server.captured_requests().len(), 4);
        assert_eq!(fsynced_count(&provider_log), 4);
        assert_eq!(fsynced_count(&effect_log), 2);
        evidence.record_command("completed Rewind", &completed);
        evidence.record_command("exact Rewind replay", &replayed);
        world.stop_daemon().unwrap();
        let stopped = daemon.wait(None).unwrap();
        evidence.record_command("stop exact replay daemon", &stopped);
        assert!(stopped.is_clean_exit(0));
        evidence.record_captures(server.captured_requests());
    }));
    support::crash::finish(evidence, result);
}

fn assert_successful_rewind(output: &CommandOutcome, target: &str, barrier: &str) {
    assert!(
        output.is_clean_exit(0),
        "completed retry after {barrier} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let success: Value = serde_json::from_slice(&output.stdout).expect("Rewind success JSON");
    assert_eq!(success["target_commit_id"], target);
    assert_eq!(success["session_head"]["new_commit_id"], target);
}

fn assert_completed_state(send: &SessionSendDriver<'_>, world: &ScenarioWorld, target: &str) {
    assert!(
        !world.project_dir().join("b.txt").exists(),
        "completed retry restored b.txt again"
    );
    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log after completed retry");
    assert!(
        log.is_clean_exit(0),
        "session log failed: {}",
        String::from_utf8_lossy(&log.stderr)
    );
    let payload: Value = serde_json::from_slice(&log.stdout).expect("session log JSON");
    let turns = payload["turns"].as_array().expect("session log turns");
    assert_eq!(turns.len(), 1, "Session head must remain at the target");
    assert_eq!(turns[0]["commit_id"], target);
}

fn committed_id(outcome: &lattice_system_harness::SendExecution) -> String {
    outcome
        .committed_result()
        .expect("fixed committed Turn")
        .1
        .to_owned()
}

fn fsynced_count(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|content| content.lines().count())
        .unwrap_or(0)
}

fn wait_for_ack(path: &std::path::Path, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        assert!(
            Instant::now() < deadline,
            "barrier ack exceeded scenario budget"
        );
        if path.is_file() {
            return serde_json::from_slice(&std::fs::read(path).expect("read barrier ack"))
                .expect("barrier ack JSON");
        }
        assert!(Instant::now() < deadline, "barrier ack was not published");
        std::thread::sleep(Duration::from_millis(5));
    }
}
