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
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        support::record_session(
            &send,
            &mut evidence,
            sid,
            serde_json::json!({"prompt":"first real input","reply":"answer","agent":"reader","provider_requests":1}),
        );
    }));
    support::finish_recorded(evidence, checked);
}

mod support;

fn cli_json(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
) -> Value {
    let out = support::cli(world, spec, args);
    evidence.record_command(args.join(" "), &out);
    assert!(out.is_clean_exit(0), "{}", out.summary());
    serde_json::from_slice(&out.stdout).unwrap()
}

fn current_history(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    evidence: &mut ScenarioEvidence,
    tid: &str,
) -> Value {
    let out = support::cli(world, spec, &["--json", "session", "turn", tid, "--follow"]);
    evidence.record_command(
        "read current root path through completed original Turn",
        &out,
    );
    assert!(out.is_clean_exit(0), "{}", out.summary());
    std::str::from_utf8(&out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .find(|record| record["type"] == "history")
        .expect("complete history")["history"]
        .clone()
}

#[test]
fn multi_step_two_agent_tree_rewind_preserves_files_and_old_turn() {
    use serde_json::json;
    let (world, spec) =
        support::direct_world("tree-two-agents", "alpha", "[\"mutate.create\"]", "");
    world.write_project_file(".lattice/agents/beta/agent.toml",
        "schema_version = 1\n[agent]\nname = \"beta\"\nmodel = \"compat:beta\"\n[prompt]\nsystem = \"Beta isolated prompt.\"\n[tools]\nenabled = [\"mutate.create\"]\n").unwrap();
    let mut server = support::wire_fake_llm(
        &world,
        "alpha",
        support::sequence(vec![
            support::tool_call(
                "create",
                "mutate.create",
                r#"{"path":"kept.txt","content":"kept\n","create_parents":false}"#,
            ),
            SseScript::plain("old final"),
            SseScript::plain("alpha fork"),
            SseScript::plain("beta root"),
        ]),
    );
    let driver = support::driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "alpha");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = send.send("original").unwrap();
        evidence.record_command("multi-step original", &first.command);
        assert!(first.command.is_clean_exit(0));
        let sid = send.session_id().unwrap();
        let tid = first.accepted_turn_id().unwrap();
        let original = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "turn", tid],
        );
        assert_eq!(original["steps"].as_array().unwrap().len(), 2);
        assert_eq!(
            original["steps"][0]["state"]["end"],
            json!({"end":"continue","node":2})
        );
        let original_history = current_history(&world, &spec, &mut evidence, tid);
        assert_eq!(original_history["anchor"]["head"], 3);
        let beta_turn = "00000000-0000-4000-8000-000000000003";
        let (mut beta, gate) = support::wire_gated_fake_llm(
            &world,
            "beta",
            support::sequence(vec![
                support::tool_call(
                    "beta-create",
                    "mutate.create",
                    r#"{"path":"cancelled.txt","content":"survives cancel\n","create_parents":false}"#,
                ),
                SseScript::plain("must not arrive"),
            ]),
        );
        let running = driver
            .start(
                &world,
                &lattice_system_harness::LatticeCommand {
                    args: [
                        "--json",
                        "session",
                        "send",
                        "beta mutation then cancel",
                        "--session",
                        sid,
                        "--agent",
                        "beta",
                        "--turn-id",
                        beta_turn,
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                    timeout: Some(world.budget(&spec).cap_timeout(spec.step_timeout())),
                },
            )
            .unwrap();
        gate.wait_until_stalled(1, spec.step_timeout()).unwrap();
        gate.release_next(spec.step_timeout()).unwrap();
        gate.wait_until_stalled(2, spec.step_timeout()).unwrap();
        let beta_running = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "turn", beta_turn],
        );
        assert_eq!(
            beta_running["steps"][0]["state"]["slots"][0]["state"]["outcome"],
            "succeeded"
        );
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("cancelled.txt")).unwrap(),
            "survives cancel\n"
        );
        let busy = support::cli(&world, &spec, &["--json", "session", "rewind", sid, "2"]);
        evidence.record_command("rewind while beta Owner executes", &busy);
        assert!(busy.is_clean_exit(1));
        assert_eq!(
            serde_json::from_slice::<Value>(&busy.stdout).unwrap()["kind"],
            "busy"
        );
        let cancel = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "cancel", sid, beta_turn],
        );
        assert_eq!(cancel["owned"], true);
        let cancelled = running.wait(None).unwrap();
        evidence.record_command("beta cancelled after real mutation", &cancelled);
        assert!(cancelled.is_clean_exit(2), "{}", cancelled.summary());
        let beta_closed = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "turn", beta_turn],
        );
        assert_eq!(beta_closed["summary"]["outcome"], "cancelled");
        assert_eq!(beta_closed["summary"]["notice"]["level"], "info");
        assert_eq!(beta_closed["steps"][0], beta_running["steps"][0]);
        assert_eq!(
            beta_closed["steps"][1]["state"]["end"],
            json!({"end":"stop","node":null,"cause":{"cause":"cancelled"}})
        );
        let beta_requests = beta.captured_requests();
        assert_eq!(beta_requests.len(), 2);
        for request in &beta_requests {
            assert_eq!(request.raw["model"], "beta");
            assert_eq!(
                request.raw["messages"][0],
                json!({"role":"system","content":"Beta isolated prompt."})
            );
        }
        evidence.record_captures(beta_requests.clone());
        beta.shutdown(spec.step_timeout()).unwrap();
        world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
        world.set_env("OPENAI_COMPATIBLE_MODEL", "alpha");
        let history_snapshot = || {
            let path = std::fs::read_dir(world.session_store_dir().join("projects"))
                .unwrap()
                .map(|entry| entry.unwrap().path().join("workspace-history.git"))
                .find(|path| path.is_dir())
                .unwrap();
            let repo = gix::open_opts(path, gix::open::Options::isolated()).unwrap();
            let current = repo
                .find_reference("refs/lattice/workspace-history/current")
                .unwrap();
            let tip = current.target().try_id().unwrap().to_owned();
            let commit = repo.find_commit(tip).unwrap();
            json!({"tip":tip.to_string(),"message":commit.message_raw_sloppy().to_string()})
        };
        let history_before = history_snapshot();
        let before = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(before["nodes"].as_array().unwrap().len(), 6);
        assert_eq!(before["current_agent"], "beta");
        let invalid = support::cli(&world, &spec, &["--json", "session", "rewind", sid, "999"]);
        evidence.record_command("nonexistent node refusal", &invalid);
        assert!(invalid.is_clean_exit(2));
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "log", sid]
            ),
            before
        );
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "rewind", sid, "2"]
            )["current"],
            2
        );
        let fork_history = current_history(&world, &spec, &mut evidence, tid);
        assert_eq!(fork_history["anchor"]["head"], 2);
        assert_eq!(
            fork_history["entries"],
            json!(&original_history["entries"].as_array().unwrap()[..2])
        );
        let fork = support::cli(
            &world,
            &spec,
            &["--json", "session", "send", "fork", "--session", sid],
        );
        evidence.record_command("fork inherits alpha at target node", &fork);
        assert!(fork.is_clean_exit(0), "{}", fork.summary());
        let tree = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(tree["nodes"].as_array().unwrap().len(), 8);
        assert_eq!(tree["nodes"][6]["node"]["id"], 6);
        assert_eq!(tree["nodes"][6]["node"]["parent"], 2);
        assert_eq!(tree["current_agent"], "alpha");
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "turn", tid]
            ),
            original
        );
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "turn", beta_turn]
            ),
            beta_closed
        );
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "rewind", sid, "3"]
            )["current"],
            3
        );
        let returned = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(returned["current_agent"], "alpha");
        assert_eq!(returned["nodes"].as_array().unwrap().len(), 8);
        assert_eq!(
            current_history(&world, &spec, &mut evidence, tid),
            original_history
        );
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "rewind", sid, "0"]
            )["current"],
            0
        );
        let root_history = current_history(&world, &spec, &mut evidence, tid);
        assert_eq!(root_history["entries"], json!([]));
        assert_eq!(root_history["anchor"]["head"], 0);
        let at_root = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert!(at_root["current_agent"].is_null());
        let refused = support::cli(
            &world,
            &spec,
            &["--json", "session", "send", "no agent", "--session", sid],
        );
        evidence.record_command("root rejects missing agent", &refused);
        assert!(refused.is_clean_exit(2));
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "log", sid]
            ),
            at_root
        );
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
        evidence.record_command("beta rewrites first message", &root);
        assert!(root.is_clean_exit(0));
        let after = cli_json(
            &world,
            &spec,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(after["nodes"].as_array().unwrap().len(), 10);
        assert_eq!(after["nodes"][8]["node"]["id"], 8);
        assert_eq!(after["nodes"][8]["node"]["parent"], 0);
        assert_eq!(after["session"]["current"], 9);
        assert_eq!(after["current_agent"], "beta");
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "turn", tid]
            ),
            original
        );
        assert_eq!(
            cli_json(
                &world,
                &spec,
                &mut evidence,
                &["--json", "session", "turn", beta_turn]
            ),
            beta_closed
        );
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("kept.txt")).unwrap(),
            "kept\n"
        );
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("cancelled.txt")).unwrap(),
            "survives cancel\n"
        );
        assert_eq!(history_snapshot(), history_before);
        let captures = server.captured_requests();
        let mut all_captures = captures[..2].to_vec();
        all_captures.extend(beta_requests);
        all_captures.extend_from_slice(&captures[2..]);
        evidence.record_captures(all_captures);
        assert_eq!(captures.len(), 4);
        assert_eq!(captures[2].raw["model"], "alpha");
        assert_eq!(captures[3].raw["model"], "beta");
        // 固定夹具的规定投影：来源是公开历史；不导入产品投影器，也不使用上一请求作 oracle。
        let step = &fork_history["entries"][1]["content"]["content"];
        let call = &step["assistant"]["parts"][0]["semantic"]["tool_call"]["call"];
        assert_eq!(call["provider_call_id"], "create");
        assert_eq!(call["name"], "mutate.create");
        assert_eq!(
            call["arguments"],
            json!({"content":"kept\n","create_parents":false,"path":"kept.txt"})
        );
        assert_eq!(
            step["results"][0]["payload"]["payload"]["value"],
            json!({"bytes_written":5,"path":"kept.txt"})
        );
        let expected = json!([
            {"role":"system","content":"You are a concise test agent."},
            {"role":"user","content":fork_history["entries"][0]["content"]["message"]["parts"][0]["semantic"]["text"]["text"]},
            {"role":"assistant","content":null,"tool_calls":[{"id":"create","type":"function","function":{
                "name":"mutate__create","arguments":serde_json::to_string(&call["arguments"]).unwrap()}}]},
            {"role":"tool","tool_call_id":"create","content":serde_json::to_string(&step["results"][0]["payload"]["payload"]["value"]).unwrap()},
            {"role":"user","content":"fork"},
        ]);
        assert_eq!(captures[2].raw["messages"], expected);
        assert_eq!(
            captures[3].raw["messages"],
            json!([{"role":"system","content":"Beta isolated prompt."},{"role":"user","content":"root input"}])
        );
        for request in &captures {
            let tools = request.raw["tools"].as_array().unwrap();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0]["function"]["name"], "mutate__create");
            assert_eq!(
                tools[0]["function"]["parameters"],
                json!({"type":"object","additionalProperties":false,
                "properties":{"content":{"type":"string"},"create_parents":{"type":"boolean"},"path":{"type":"string","minLength":1}},
                "required":["path","content"]})
            );
        }
        for args in [
            vec!["session", "log", sid],
            vec!["session", "turn", tid],
            vec!["--json", "session", "list"],
            vec!["--json", "audit"],
            vec!["--json", "audit", "--tail", "--since", "0"],
            vec!["--json", "audit", "replay", "--session", sid, "--turn", tid],
            vec![
                "--json",
                "audit",
                "replay",
                "--session",
                sid,
                "--turn",
                beta_turn,
            ],
        ] {
            let out = support::cli(&world, &spec, &args);
            evidence.record_command(args.join(" "), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
        }
        evidence
            .record_json("history-before-fork", &fork_history)
            .unwrap();
        evidence
            .record_json("expected-fork-projection", &expected)
            .unwrap();
        evidence
            .record_json(
                "workspace-history",
                &json!({"before_rewind":history_before,"after_root_fork":history_snapshot()}),
            )
            .unwrap();
        evidence.record_json("state-summary", &json!({"original":original,"cancelled":beta_closed,"tree":after,
            "state_root":world.session_store_dir().parent(),"daemon_pids":world.daemon_pids().unwrap(),
            "files":(["kept.txt","cancelled.txt"].map(|path| json!({"path":path,"blake3":blake3::hash(&std::fs::read(world.project_dir().join(path)).unwrap()).to_hex().to_string()}))),
            "provider_requests":6,"script":"alpha create kept.txt then final; beta create cancelled.txt then cancel at request2; alpha fork; beta root"})).unwrap();
        server.shutdown(spec.step_timeout()).unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn legacy_stores_are_refused_before_any_new_database_or_request() {
    fn inventory(root: &std::path::Path) -> std::collections::BTreeMap<String, String> {
        fn visit(
            root: &std::path::Path,
            path: &std::path::Path,
            entries: &mut std::collections::BTreeMap<String, String>,
        ) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                if entry.file_type().unwrap().is_dir() {
                    entries.insert(relative, "directory".into());
                    visit(root, &path, entries);
                } else {
                    entries.insert(
                        relative,
                        blake3::hash(&std::fs::read(&path).unwrap())
                            .to_hex()
                            .to_string(),
                    );
                }
            }
        }
        let mut entries = std::collections::BTreeMap::new();
        visit(root, root, &mut entries);
        entries
    }
    let session = "sessions.git";
    let invocation =
        "projects/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/invocations.git";
    for relatives in [vec![session], vec![invocation], vec![session, invocation]] {
        let (world, spec) = support::direct_world("legacy-store-refusal", "reader", "[]", "");
        let mut server =
            support::wire_fake_llm(&world, "reader", SseScript::plain("must not dispatch"));
        let mut legacies: Vec<_> = relatives
            .iter()
            .map(|relative| world.session_store_dir().join(relative))
            .collect();
        legacies.sort();
        for legacy in &legacies {
            std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            gix::init_bare(legacy).unwrap();
        }
        let before: Vec<_> = legacies.iter().map(|path| inventory(path)).collect();
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
                assert_eq!(error["store"]["paths"], serde_json::json!(legacies));
                assert!(server.captured_requests().is_empty());
                assert!(!world.session_store_dir().join("lattice.redb").exists());
                assert_eq!(
                    legacies
                        .iter()
                        .map(|path| inventory(path))
                        .collect::<Vec<_>>(),
                    before
                );
            }
            evidence.record_json("state-summary", &serde_json::json!({
                "legacy_paths":legacies,"before":before,"after":legacies.iter().map(|p| inventory(p)).collect::<Vec<_>>(),
                "provider_requests":0,"database_created":false,"state_root":world.session_store_dir().parent(),
                "log_turn":"not applicable: no Session was accepted", "script":"plain must not dispatch"
            })).unwrap();
            evidence.record_captures(server.captured_requests());
            server.shutdown(spec.step_timeout()).unwrap();
        }));
        support::finish_recorded(evidence, result);
    }
}
