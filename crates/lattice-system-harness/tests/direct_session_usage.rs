// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 公开用量输出只累计当前位置的父链，读取与后台重开保持已存事实。

mod support;

use lattice_system_harness::{
    LatticeCommand, NativeUsage, ScenarioEvidence, SessionSendDriver, SseScript,
};
use serde_json::{Value, json};

fn query(send: &SessionSendDriver<'_>, evidence: &mut ScenarioEvidence, args: &[&str]) -> Value {
    let out = send.run_command(args).unwrap();
    evidence.record_command(args.join(" "), &out);
    assert!(out.is_clean_exit(0), "{}", out.summary());
    serde_json::from_slice(&out.stdout).unwrap()
}

fn usage(send: &SessionSendDriver<'_>, evidence: &mut ScenarioEvidence, sid: &str) -> Value {
    query(send, evidence, &["--json", "session", "usage", sid])
}

fn metric(known: Value, unknown: u64) -> Value {
    json!({"known":known,"unknown_attempts":unknown,"overflowed":false})
}

fn assert_summary(actual: &Value, expected: &Value) {
    for field in [
        "attempts",
        "kind",
        "input",
        "cache_read",
        "cache_write",
        "output",
        "reasoning",
    ] {
        assert_eq!(actual[field], expected[field], "{field}: {actual}");
    }
    let cost = &actual["estimated_cost_usd"];
    let expected = &expected["estimated_cost_usd"];
    assert_eq!(cost["unknown_attempts"], expected["unknown_attempts"]);
    assert_eq!(cost["overflowed"], expected["overflowed"]);
    match (cost["known"].as_f64(), expected["known"].as_f64()) {
        (Some(actual), Some(expected)) => assert!((actual - expected).abs() < 1e-12, "{cost}"),
        _ => assert_eq!(cost["known"], expected["known"]),
    }
}

fn assert_text(send: &SessionSendDriver<'_>, evidence: &mut ScenarioEvidence, snapshot: &Value) {
    let sid = snapshot["session_id"].as_str().unwrap();
    let out = send.run_command(&["session", "usage", sid]).unwrap();
    evidence.record_command("text usage", &out);
    assert!(out.is_clean_exit(0), "{}", out.summary());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.split_whitespace().any(|word| word == sid), "{text}");
    let lines: Vec<Vec<_>> = text
        .lines()
        .map(|line| line.split_whitespace().collect())
        .collect();
    let turns = snapshot["turns"].as_array().unwrap();
    assert_eq!(
        lines
            .iter()
            .filter(|row| row.first().is_some_and(|word| word.parse::<u64>().is_ok()))
            .count(),
        turns.len(),
        "{text}"
    );
    for turn in turns {
        let id = turn["turn_id"].as_str().unwrap();
        let row = lines
            .iter()
            .find(|row| row.get(1) == Some(&id))
            .expect("text Turn row");
        assert_eq!(row[0].parse::<u64>().unwrap(), turn["ordinal"]);
        assert_eq!(
            row[2],
            format!(
                "{}/{}",
                turn["provider"].as_str().unwrap(),
                turn["model"].as_str().unwrap()
            )
        );
        assert_text_metrics(&row[3..], &turn["usage"]);
    }
    let total = lines
        .iter()
        .find(|row| row.first() == Some(&"total"))
        .expect("text total row");
    assert_text_metrics(&total[3..], &snapshot["total"]);
}

fn assert_text_metrics(words: &[&str], summary: &Value) {
    let mut words = words.iter().copied();
    assert_eq!(
        words.next().unwrap().parse::<u64>().unwrap(),
        summary["attempts"]
    );
    for field in [
        "input",
        "cache_read",
        "cache_write",
        "output",
        "reasoning",
        "estimated_cost_usd",
    ] {
        let metric = &summary[field];
        let text = words.next().unwrap();
        if metric["known"].is_null() {
            assert_eq!(text, "-", "unknown {field}");
            continue;
        }
        if field == "estimated_cost_usd" {
            let amount = text.strip_prefix("≈$").expect("estimated USD marker");
            let (whole, cents) = amount.split_once('.').expect("USD decimal point");
            assert!(!whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(cents.len(), 2, "USD uses two decimal places: {text}");
            assert!(cents.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(
                amount.parse::<f64>().unwrap(),
                (metric["known"].as_f64().unwrap() * 100.0).round() / 100.0
            );
        } else {
            assert_eq!(text.parse::<u64>().unwrap(), metric["known"], "{field}");
        }
        if metric["unknown_attempts"].as_u64().unwrap() > 0 {
            assert_eq!(words.next(), Some("(partial)"), "partial {field}");
        }
    }
    assert_eq!(words.next(), None, "unexpected metric or marker");
}

fn with_usage(script: SseScript, usage: Value) -> SseScript {
    SseScript::WithUsage {
        script: Box::new(script),
        usage: NativeUsage::ChatCompletions(usage),
    }
}

#[test]
fn usage_follows_current_path_and_survives_read_only_reopening() {
    let (world, spec) = support::direct_world(
        "session-usage",
        "priced",
        "[\"observe__workspace\"]",
        "[llm.retry]\nmax_retries = 1\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let driver = support::driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "priced");
    world
        .write_project_file("notes.txt", "usage tool result\n")
        .unwrap();
    world
        .write_agent_profile("unpriced", "local/unpriced")
        .unwrap();
    let full_usage = json!({"prompt_tokens":1000,"completion_tokens":100,
        "prompt_tokens_details":{"cached_tokens":200,"cache_creation_input_tokens":100},
        "completion_tokens_details":{"reasoning_tokens":20}});
    let (server, gate) = support::wire_gated_fake_llm(
        &world,
        "priced",
        support::sequence(vec![
            SseScript::Chunks {
                chunks: vec![
                    json!({"choices":[{"delta":{"content":"failed partial"}}],"usage":full_usage}),
                    json!({"error":{"type":"server_error","message":"retry this attempt"}}),
                ],
            },
            with_usage(
                support::tool_call(
                    "read",
                    "observe__workspace",
                    r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
                ),
                json!({"prompt_tokens":1000,"completion_tokens":100,"prompt_tokens_details":{"cached_tokens":200},"completion_tokens_details":{"reasoning_tokens":0}}),
            ),
            with_usage(SseScript::plain("original final"), full_usage),
            with_usage(
                SseScript::plain("unpriced second turn"),
                json!({"prompt_tokens":500,"completion_tokens":50}),
            ),
            with_usage(
                SseScript::plain("another session"),
                json!({"prompt_tokens":9000,"completion_tokens":9000}),
            ),
            with_usage(
                SseScript::plain("zero branch"),
                json!({"prompt_tokens":0,"completion_tokens":0,"prompt_tokens_details":{"cached_tokens":0,"cache_creation_input_tokens":0},"completion_tokens_details":{"reasoning_tokens":0}}),
            ),
        ]),
    );
    let configuration = world.home().join(".lattice/providers.toml");
    let baseline = std::fs::read_to_string(&configuration).unwrap();
    let set_price = |prices: &str| {
        std::fs::write(
            &configuration,
            format!("{baseline}\n[providers.local.models.priced.cost]\n{prices}\n"),
        )
        .unwrap()
    };
    let prices = "input = 2\noutput = 10\ncache_read = 0.5\ncache_write = 3";
    set_price(prices);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let running = send.spawn("read notes after retry").unwrap();
        gate.wait_until_stalled(1, running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        let sessions = query(&send, &mut evidence, &["--json", "session", "list"]);
        let sid = sessions[0]["session_id"].as_str().unwrap().to_owned();
        let live = usage(&send, &mut evidence, &sid);
        let a = live["turns"][0]["turn_id"].as_str().unwrap().to_owned();
        assert_eq!(live["total"]["attempts"], 1);
        assert!(live["total"]["input"]["known"].is_null());
        assert!(live["total"]["estimated_cost_usd"]["known"].is_null());
        assert_text(&send, &mut evidence, &live);
        assert_eq!(server.captured_requests().len(), 1);
        gate.release_next(running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        gate.wait_until_stalled(2, running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        set_price("input = 2");
        gate.release_next(running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        gate.wait_until_stalled(3, running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        set_price(prices);
        gate.release_next(running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        let original = send.wait_spawned(running).unwrap();
        evidence.record_command("original multi-step turn", &original.command);
        assert!(
            original.command.is_clean_exit(0),
            "{}",
            original.command.summary()
        );
        assert_eq!(original.accepted_session_id(), Some(sid.as_str()));
        let original_turn = query(&send, &mut evidence, &["--json", "session", "turn", &a]);
        assert_eq!(original_turn["steps"].as_array().unwrap().len(), 2);
        assert_eq!(
            original_turn["steps"][0]["attempts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        for attempt in [
            &original_turn["steps"][0]["attempts"][0],
            &original_turn["steps"][1]["attempts"][0],
        ] {
            assert!((attempt["estimated_cost_usd"].as_f64().unwrap() - 0.0028).abs() < 1e-12);
        }
        assert!(original_turn["steps"][0]["attempts"][1]["estimated_cost_usd"].is_null());

        let running = driver
            .start(
                &world,
                &LatticeCommand {
                    args: [
                        "--json",
                        "session",
                        "send",
                        "second turn",
                        "--session",
                        &sid,
                        "--agent",
                        "unpriced",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                    timeout: Some(world.budget(&spec).cap_timeout(spec.step_timeout())),
                },
            )
            .unwrap();
        gate.wait_until_stalled(4, running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        gate.release_next(running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        let second = running.wait(None).unwrap();
        evidence.record_command("unpriced second turn", &second);
        assert!(second.is_clean_exit(0), "{}", second.summary());
        let b = support::parse_send_stdout(&second.stdout, second.exit_code())
            .accepted_turn_id()
            .unwrap()
            .to_owned();
        let full = usage(&send, &mut evidence, &sid);
        assert_eq!(full["position"], 5);
        assert_eq!(full["turns"].as_array().unwrap().len(), 2);
        for (index, (tid, model)) in [(&a, "priced"), (&b, "unpriced")].into_iter().enumerate() {
            let row = &full["turns"][index];
            assert_eq!(row["turn_id"], *tid);
            assert_eq!(row["ordinal"], index + 1);
            assert_eq!(row["provider"], "local");
            assert_eq!(row["model"], model);
        }
        let a_summary = json!({"attempts":3,"kind":"partial",
            "input":metric(json!(3000),0),"cache_read":metric(json!(600),0),"cache_write":metric(json!(200),1),
            "output":metric(json!(300),0),"reasoning":metric(json!(40),0),"estimated_cost_usd":metric(json!(0.0056),1)});
        let b_summary = json!({"attempts":1,"kind":"partial",
            "input":metric(json!(500),0),"cache_read":metric(Value::Null,1),"cache_write":metric(Value::Null,1),
            "output":metric(json!(50),0),"reasoning":metric(Value::Null,1),"estimated_cost_usd":metric(Value::Null,1)});
        let full_summary = json!({"attempts":4,"kind":"partial",
            "input":metric(json!(3500),0),"cache_read":metric(json!(600),1),"cache_write":metric(json!(200),2),
            "output":metric(json!(350),0),"reasoning":metric(json!(40),1),"estimated_cost_usd":metric(json!(0.0056),2)});
        assert_summary(&full["turns"][0]["usage"], &a_summary);
        assert_summary(&full["turns"][1]["usage"], &b_summary);
        assert_summary(&full["total"], &full_summary);
        assert_text(&send, &mut evidence, &full);

        let other = SessionSendDriver::new(&driver, &world, &spec, "unpriced");
        let running = other.spawn("isolated session").unwrap();
        gate.wait_until_stalled(5, running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        gate.release_next(running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        let outcome = other.wait_spawned(running).unwrap();
        evidence.record_command("other session", &outcome.command);
        assert!(outcome.command.is_clean_exit(0));
        assert_ne!(other.session_id().unwrap(), sid);
        assert_eq!(usage(&send, &mut evidence, &sid), full);

        query(
            &send,
            &mut evidence,
            &["--json", "session", "rewind", &sid, "2"],
        );
        let middle = usage(&send, &mut evidence, &sid);
        assert_eq!(middle["position"], 2);
        assert_eq!(middle["turns"].as_array().unwrap().len(), 1);
        let middle_summary = json!({"attempts":2,"kind":"partial",
            "input":metric(json!(2000),0),"cache_read":metric(json!(400),0),"cache_write":metric(json!(100),1),
            "output":metric(json!(200),0),"reasoning":metric(json!(20),0),"estimated_cost_usd":metric(json!(0.0028),1)});
        assert_summary(&middle["turns"][0]["usage"], &middle_summary);
        assert_summary(&middle["total"], &middle_summary);
        assert_text(&send, &mut evidence, &middle);
        let running = send.spawn("new branch").unwrap();
        gate.wait_until_stalled(6, running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        gate.release_next(running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        let branch = send.wait_spawned(running).unwrap();
        evidence.record_command("new branch", &branch.command);
        assert!(branch.command.is_clean_exit(0));
        let c = branch.accepted_turn_id().unwrap();
        let fork = usage(&send, &mut evidence, &sid);
        assert_eq!(fork["position"], 7);
        assert_eq!(fork["turns"].as_array().unwrap().len(), 2);
        assert_eq!(fork["turns"][1]["turn_id"], c);
        assert_eq!(fork["turns"][1]["ordinal"], 2);
        let zero = json!({"attempts":1,"kind":"complete",
            "input":metric(json!(0),0),"cache_read":metric(json!(0),0),"cache_write":metric(json!(0),0),
            "output":metric(json!(0),0),"reasoning":metric(json!(0),0),"estimated_cost_usd":metric(json!(0.0),0)});
        assert_summary(&fork["turns"][0]["usage"], &middle_summary);
        assert_summary(&fork["turns"][1]["usage"], &zero);
        let mut fork_summary = middle_summary.clone();
        fork_summary["attempts"] = json!(3);
        assert_summary(&fork["total"], &fork_summary);
        assert_text(&send, &mut evidence, &fork);

        query(
            &send,
            &mut evidence,
            &["--json", "session", "rewind", &sid, "5"],
        );
        assert_eq!(usage(&send, &mut evidence, &sid), full);
        std::fs::write(
            &configuration,
            "[metadata]\nauto_refresh=false\n[providers.local]\nprotocol='unavailable'\n",
        )
        .unwrap();
        std::fs::remove_file(world.home().join(".lattice/auth.json")).unwrap();
        std::fs::remove_dir_all(world.project_dir().join(".lattice/agents")).unwrap();
        let epoch = query(&send, &mut evidence, &["--json", "daemon", "status"])["epoch"].clone();
        let stopped = send.run_command(&["daemon", "stop"]).unwrap();
        evidence.record_command("stop before read-only reopening", &stopped);
        assert!(stopped.is_clean_exit(0));
        world.stop_daemon().unwrap();
        assert_eq!(usage(&send, &mut evidence, &sid), full);
        assert_ne!(
            query(&send, &mut evidence, &["--json", "daemon", "status"])["epoch"],
            epoch
        );
        assert_eq!(
            query(&send, &mut evidence, &["--json", "session", "turn", &a]),
            original_turn
        );
        let mut empty = zero;
        empty["attempts"] = json!(0);
        query(
            &send,
            &mut evidence,
            &["--json", "session", "rewind", &sid, "1"],
        );
        let user_only = usage(&send, &mut evidence, &sid);
        assert_eq!(user_only["turns"].as_array().unwrap().len(), 1);
        assert_eq!(user_only["turns"][0]["turn_id"], a);
        assert_summary(&user_only["turns"][0]["usage"], &empty);
        assert_summary(&user_only["total"], &empty);
        query(
            &send,
            &mut evidence,
            &["--json", "session", "rewind", &sid, "0"],
        );
        let root = usage(&send, &mut evidence, &sid);
        assert_eq!(root["position"], 0);
        assert_eq!(root["turns"], json!([]));
        assert_summary(&root["total"], &empty);
        assert_text(&send, &mut evidence, &root);
        assert_eq!(
            query(&send, &mut evidence, &["--json", "session", "turn", &a]),
            original_turn
        );
        assert_eq!(server.captured_requests().len(), 6);
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}
