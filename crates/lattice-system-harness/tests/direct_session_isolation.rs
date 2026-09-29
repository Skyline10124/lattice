// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 两个 Session 的树、当前位置和后续模型上下文独立。
mod support;
use lattice_system_harness::{SessionSendDriver, SseScript};
use serde_json::Value;
use support::*;

#[test]
fn direct_session_tree_rewind_and_context_isolation() {
    let driver = driver();
    let (world, spec) = direct_world("direct-tree-isolation", "isolation", "[]", "");
    let server = wire_fake_llm(&world, "isolation", SseScript::plain("answer"));
    let mut evidence = evidence(&world, &spec);
    let a = SessionSendDriver::new(&driver, &world, &spec, "isolation");
    let b = SessionSendDriver::new(&driver, &world, &spec, "isolation");
    let a1 = a.send("a1").unwrap();
    let a2 = a.send("a2").unwrap();
    let b1 = b.send("b1").unwrap();
    let b2 = b.send("b2").unwrap();
    for (label, turn) in [("a1", &a1), ("a2", &a2), ("b1", &b1), ("b2", &b2)] {
        evidence.record_command(label, &turn.command);
    }
    let sid_a = a.session_id().unwrap();
    let sid_b = b.session_id().unwrap();
    let result = (|| {
        for turn in [&a1, &a2, &b1, &b2] {
            assert_clean_send(turn)?;
            require(turn.has_completed(), "seed Turn completed")?;
        }
        require(sid_a != sid_b, "independent Session identity")?;
        let mut read = |args: &[&str]| -> Value {
            let out = cli(&world, &spec, args);
            evidence.record_command(args.join(" "), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
            serde_json::from_slice(&out.stdout).unwrap()
        };
        let before_a = read(&["--json", "session", "log", sid_a]);
        let before_b = read(&["--json", "session", "log", sid_b]);
        require(
            before_a["nodes"].as_array().unwrap().len() == 5
                && before_b["nodes"].as_array().unwrap().len() == 5,
            "each tree has root and two Turns",
        )?;
        let a_detail = read(&["--json", "session", "turn", a2.accepted_turn_id().unwrap()]);
        let moved = read(&["--json", "session", "rewind", sid_a, "2"]);
        require(moved["current"] == 2, "rewind targets node itself")?;
        require(
            read(&["--json", "session", "log", sid_b]) == before_b,
            "rewind A cannot change B",
        )?;
        let a3 = a.send("a3 fork").unwrap();
        assert_clean_send(&a3)?;
        let after_a = read(&["--json", "session", "log", sid_a]);
        require(
            after_a["nodes"].as_array().unwrap().len() == 7
                && after_a["nodes"][5]["node"]["parent"] == 2
                && after_a["nodes"][5]["node"]["id"] == 5
                && after_a["session"]["current"] == 6,
            "fork appends max+1 and preserves old branch",
        )?;
        require(
            read(&["--json", "session", "turn", a2.accepted_turn_id().unwrap()]) == a_detail,
            "off-path Turn detail immutable",
        )?;
        require(
            read(&["--json", "session", "log", sid_b]) == before_b,
            "A fork cannot change B",
        )?;
        let invalid = cli(&world, &spec, &["--json", "session", "rewind", sid_b, "6"]);
        require(
            invalid.is_clean_exit(2),
            "A-only NodeId cannot be resolved in B",
        )?;
        require(
            read(&["--json", "session", "log", sid_b]) == before_b,
            "invalid rewind leaves B unchanged",
        )?;
        let b3 = b.send("b3").unwrap();
        assert_clean_send(&b3)?;
        let captures = server.captured_requests();
        require(
            captures.len() == 6,
            "queries and rewind never execute provider",
        )?;
        let messages = |i: usize| {
            captures[i].raw["messages"]
                .as_array()
                .unwrap()
                .iter()
                .skip(1)
                .map(|m| m["content"].as_str().unwrap())
                .collect::<Vec<_>>()
        };
        require(
            messages(4) == ["a1", "answer", "a3 fork"]
                && messages(5) == ["b1", "answer", "b2", "answer", "b3"],
            "provider contexts use each Session's own root path",
        )?;
        evidence.record_command("A fork", &a3.command);
        evidence.record_command("invalid B rewind", &invalid);
        evidence.record_command("B continues", &b3.command);
        Ok(())
    })();
    finish_scenario(evidence, &[], server.captured_requests(), result);
}
