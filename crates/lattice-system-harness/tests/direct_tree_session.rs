// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_system_harness::{
    FAKE_API_KEY, FakeLlmServer, LatticeDriver, ScenarioEvidence, ScenarioSpec, ScenarioWorld,
    SessionSendDriver, SseScript,
};
use serde_json::Value;

#[test]
fn first_prompt_is_the_only_session_creation_and_request() {
    let spec = ScenarioSpec::direct_local("r2-first-prompt");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world
        .write_agent_profile("reader", "compat:reader")
        .unwrap();
    world.init_git_project().unwrap();
    let mut provider = FakeLlmServer::start(SseScript::plain("answer")).unwrap();
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", provider.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", "reader");
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    let driver = LatticeDriver::from_env().unwrap();
    let mut evidence =
        ScenarioEvidence::new(&world, &spec.name, [FAKE_API_KEY.to_owned()]).unwrap();
    let send = SessionSendDriver::new(&driver, &world, &spec, "reader");
    assert_eq!(send.session_id(), None);
    assert!(
        !world.session_store_dir().join("lattice.redb").exists(),
        "construction must not create a Session"
    );
    let result = send.send("first real input").unwrap();
    evidence.record_command("first send", &result.command);
    assert!(result.command.is_clean_exit(0), "{:?}", result.command);
    assert!(
        result.malformed_lines.is_empty(),
        "{:?}",
        result.malformed_lines
    );
    let sid = result.accepted_session_id().expect("accepted SID");
    let tid = result.accepted_turn_id().expect("accepted TurnId");
    let log = send
        .run_command(&["--json", "session", "log", sid])
        .unwrap();
    evidence.record_command("tree log", &log);
    assert!(log.is_clean_exit(0));
    let tree: Value = serde_json::from_slice(&log.stdout).unwrap();
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 3);
    let detail = send
        .run_command(&["--json", "session", "turn", tid])
        .unwrap();
    evidence.record_command("turn detail", &detail);
    assert!(detail.is_clean_exit(0));
    let detail: Value = serde_json::from_slice(&detail.stdout).unwrap();
    assert_eq!(detail["summary"]["outcome"], "completed");
    assert_eq!(detail["steps"].as_array().unwrap().len(), 1);
    let captures = provider.captured_requests();
    assert_eq!(captures.len(), 1, "no bootstrap request");
    assert_eq!(
        captures[0].raw["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["content"],
        "first real input"
    );
    evidence.record_captures(captures);
    provider.shutdown(spec.step_timeout()).unwrap();
    evidence.finish_report(None).unwrap();
}

mod support;

#[test]
fn multi_step_two_agent_tree_rewind_preserves_files_and_old_turn() {
    let (world, spec) =
        support::direct_world("tree-two-agents", "alpha", "[\"mutate.create\"]", "");
    world.write_agent_profile("beta", "compat:beta").unwrap();
    let server = support::wire_fake_llm(
        &world,
        "alpha",
        SseScript::Sequence {
            scripts: vec![
                support::tool_call(
                    "create",
                    "mutate.create",
                    r#"{"path":"kept.txt","content":"kept\n","create_parents":false}"#,
                ),
                SseScript::plain("old final"),
                SseScript::plain("beta fork"),
                SseScript::plain("beta root"),
            ],
        },
    );
    let driver = support::driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "alpha");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = send.send("original").unwrap();
        evidence.record_command("multi-step original", &first.command);
        support::assert_clean_send(&first).unwrap();
        let sid = send.session_id().unwrap();
        let tid = first.accepted_turn_id().unwrap();
        let mut read = |args: &[&str]| -> Value {
            let out = support::cli(&world, &spec, args);
            evidence.record_command(args.join(" "), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
            serde_json::from_slice(&out.stdout).unwrap()
        };
        let original = read(&["--json", "session", "turn", tid]);
        assert_eq!(original["steps"].as_array().unwrap().len(), 2);
        assert_eq!(
            original["steps"][0]["state"]["end"],
            serde_json::json!({"end":"continue","node":2})
        );
        let before = read(&["--json", "session", "log", sid]);
        assert_eq!(before["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(
            read(&["--json", "session", "rewind", sid, "2"])["current"],
            2
        );
        let fork = support::cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "fork",
                "--session",
                sid,
                "--agent",
                "beta",
            ],
        );
        assert!(fork.is_clean_exit(0), "{}", fork.summary());
        let tree = read(&["--json", "session", "log", sid]);
        assert_eq!(tree["nodes"].as_array().unwrap().len(), 6);
        assert_eq!(tree["nodes"][4]["node"]["id"], 4);
        assert_eq!(tree["nodes"][4]["node"]["parent"], 2);
        assert_eq!(tree["current_agent"], "beta");
        assert_eq!(read(&["--json", "session", "turn", tid]), original);
        assert_eq!(
            read(&["--json", "session", "rewind", sid, "0"])["current"],
            0
        );
        let refused = support::cli(
            &world,
            &spec,
            &["--json", "session", "send", "no agent", "--session", sid],
        );
        assert!(refused.is_clean_exit(2));
        let at_root = read(&["--json", "session", "log", sid]);
        assert_eq!(at_root["nodes"].as_array().unwrap().len(), 6);
        assert!(at_root["current_agent"].is_null());
        assert_eq!(server.captured_requests().len(), 3);
        let root = support::cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "root input",
                "--session",
                sid,
                "--agent",
                "beta",
            ],
        );
        assert!(root.is_clean_exit(0));
        let after = read(&["--json", "session", "log", sid]);
        assert_eq!(after["nodes"].as_array().unwrap().len(), 8);
        assert_eq!(after["nodes"][6]["node"]["id"], 6);
        assert_eq!(after["nodes"][6]["node"]["parent"], 0);
        assert_eq!(after["session"]["current"], 7);
        assert_eq!(read(&["--json", "session", "turn", tid]), original);
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("kept.txt")).unwrap(),
            "kept\n"
        );
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 4);
        assert_eq!(captures[2].raw["model"], "beta");
        assert!(
            !captures[2].raw["messages"]
                .to_string()
                .contains("old final")
        );
        assert_eq!(captures[3].raw["messages"].as_array().unwrap().len(), 2);
        assert_eq!(captures[3].raw["messages"][1]["content"], "root input");
        evidence.record_command("beta fork", &fork);
        evidence.record_command("root no agent refused", &refused);
        evidence.record_command("beta root", &root);
    }));
    evidence.record_captures(server.captured_requests());
    evidence
        .finish_report(result.err().map(|p| {
            p.downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "tree assertion".into())
        }))
        .unwrap();
}

#[test]
fn legacy_stores_are_refused_before_any_new_database_or_request() {
    for relative in [
        "sessions.git",
        "projects/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/invocations.git",
    ] {
        let (world, spec) = support::direct_world("legacy-store-refusal", "reader", "[]", "");
        let server =
            support::wire_fake_llm(&world, "reader", SseScript::plain("must not dispatch"));
        let legacy = world.session_store_dir().join(relative);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        gix::init_bare(&legacy).unwrap();
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for args in [
                vec!["--json", "session", "send", "input", "--agent", "reader"],
                vec!["--json", "session", "list"],
                vec!["--json", "audit", "query"],
            ] {
                let out = support::cli(&world, &spec, &args);
                evidence.record_command(args.join(" "), &out);
                assert!(out.is_clean_exit(1), "{}", out.summary());
                assert!(out.stderr.is_empty());
                let records: Vec<Value> = std::str::from_utf8(&out.stdout)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                let error = records.last().unwrap();
                assert_eq!(error["kind"], "store_unavailable");
                assert_eq!(error["store"]["type"], "legacy_stores");
                assert!(
                    error["store"]["paths"]
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!(legacy))
                );
                assert!(server.captured_requests().is_empty());
                assert!(!world.session_store_dir().join("lattice.redb").exists());
            }
        }));
        evidence
            .finish_report(result.err().map(|p| {
                p.downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "legacy refusal assertion".into())
            }))
            .unwrap();
    }
}
