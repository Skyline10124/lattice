// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 公开树/详情与 provider 上下文互证；接受前拒绝不建立 Session。
mod support;
use lattice_system_harness::{
    ScenarioSpec, ScenarioWorld, SessionSendDriver, SseScript, TurnEventLine,
};
use serde_json::Value;
use support::*;

#[test]
fn direct_turn_completed_reopen_contract() {
    let driver = driver();
    let (world, spec) = direct_world("direct-turn-contract", "dl-contract", "[]", "");
    let server = wire_fake_llm(
        &world,
        "dl-contract",
        sequence(vec![
            SseScript::plain("first answer"),
            SseScript::plain("second answer"),
        ]),
    );
    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-contract");
    let first = send.send("first prompt").unwrap();
    let sid = send.session_id().unwrap();
    let before = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "turn",
            first.accepted_turn_id().unwrap(),
        ],
    );
    let second = send.send("second prompt").unwrap();
    let after = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "turn",
            first.accepted_turn_id().unwrap(),
        ],
    );
    let log = cli(&world, &spec, &["--json", "session", "log", sid]);
    let human = cli(&world, &spec, &["session", "log", sid]);
    let list = cli(&world, &spec, &["--json", "session", "list"]);
    let missing = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "send",
            "hello",
            "--session",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
    );
    let result = (|| {
        for turn in [&first, &second] {
            assert_clean_send(turn)?;
            assert_accepted_and_closed(turn)?;
            require(
                turn.has_completed() && turn.command.is_clean_exit(0),
                "Completed and exit0",
            )?;
        }
        require(
            before.is_clean_exit(0) && after.is_clean_exit(0) && before.stdout == after.stdout,
            "closed Turn unchanged after new Turn",
        )?;
        let captures = server.captured_requests();
        require(
            captures.len() == 2 && captures.iter().all(|c| c.model == "dl-contract"),
            "two correctly routed requests",
        )?;
        let messages = &captures[1].raw["messages"];
        require(
            messages.as_array().unwrap().len() == 4
                && messages[0]["role"] == "system"
                && messages[1]["role"] == "user"
                && messages[1]["content"] == "first prompt"
                && messages[2]["role"] == "assistant"
                && messages[2]["content"] == "first answer"
                && messages[3]["role"] == "user"
                && messages[3]["content"] == "second prompt",
            "exact prior pair precedes new input",
        )?;
        let tree: Value = serde_json::from_slice(&log.stdout).unwrap();
        require(
            log.is_clean_exit(0)
                && tree["nodes"].as_array().unwrap().len() == 5
                && tree["session"]["current"] == 4,
            "complete tree and current",
        )?;
        for turn in [&first, &second] {
            require(
                tree["nodes"].as_array().unwrap().iter().any(|r| {
                    r["turn_start"]["turn_id"] == turn.accepted_turn_id().unwrap()
                        && r["turn_start"]["summary"]["outcome"] == "completed"
                }),
                "tree preserves both Turn summaries",
            )?;
        }
        let text = String::from_utf8_lossy(&human.stdout);
        require(
            human.is_clean_exit(0)
                && text.contains(first.accepted_turn_id().unwrap())
                && text.contains(second.accepted_turn_id().unwrap())
                && text.contains("current=4"),
            "human tree exposes identity and current",
        )?;
        let listed: Value = serde_json::from_slice(&list.stdout).unwrap();
        require(
            list.is_clean_exit(0)
                && listed.as_array().unwrap().len() == 1
                && listed[0]["node_count"] == 5
                && listed[0]["active_turn"].is_null(),
            "list reflects one closed Session",
        )?;
        let error: Value = serde_json::from_slice(&missing.stdout).unwrap();
        require(
            missing.is_clean_exit(2) && error["type"] == "error" && error["kind"] == "not_found",
            "missing Session produces only typed lookup error",
        )
    })();
    for (label, out) in [
        ("first", &first.command),
        ("before", &before),
        ("second", &second.command),
        ("after", &after),
        ("tree", &log),
        ("human tree", &human),
        ("list", &list),
        ("missing", &missing),
    ] {
        evidence.record_command(label, out);
    }
    finish_scenario(evidence, &[], server.captured_requests(), result);
}

#[test]
fn direct_turn_sonnet_routes_but_missing_credentials_reject_before_acceptance() {
    let driver = driver();
    let spec = ScenarioSpec::direct_local("direct-turn-sonnet-route");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.write_agent_profile("dl-sonnet", "sonnet").unwrap();
    world.init_git_project().unwrap();
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-sonnet");
    let resolved = send.run_command(&["--json", "resolve", "sonnet"]).unwrap();
    let outcome = send.send("route sonnet").unwrap();
    let list = send.run_command(&["--json", "session", "list"]).unwrap();
    let result = (|| {
        assert_clean_send(&outcome)?;
        let routing: Value = serde_json::from_slice(&resolved.stdout).unwrap();
        require(
            resolved.is_clean_exit(0)
                && routing["provider"] == "Anthropic"
                && routing["alias"] == "sonnet"
                && routing["api_key_env"] == "ANTHROPIC_API_KEY"
                && routing["credential_status"] == "Missing",
            "formal sonnet routing with missing credential",
        )?;
        require(
            outcome.command.is_clean_exit(2)
                && outcome.accepted_turn_id().is_none()
                && send.session_id().is_none(),
            "reject before Session acceptance",
        )?;
        require(outcome.events.iter().all(|e| matches!(e, TurnEventLine::Unknown { type_name, .. } if matches!(type_name.as_str(), "submission" | "error" | "connection"))), "no model/Tool activity")?;
        let error: Value = serde_json::from_slice(&list.stdout).unwrap();
        require(
            list.is_clean_exit(1)
                && error["kind"] == "store_unavailable"
                && !world.session_store_dir().join("lattice.redb").exists(),
            "no empty store or Session after rejection",
        )
    })();
    finish_scenario(
        evidence,
        &[
            ("resolve", &resolved),
            ("rejection", &outcome.command),
            ("list", &list),
        ],
        vec![],
        result,
    );
}
