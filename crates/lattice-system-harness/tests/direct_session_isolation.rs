// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 两个会话的树结构、当前位点与后续模型上下文彼此独立。
mod support;
use lattice_system_harness::{SessionSendDriver, SseScript};
use serde_json::Value;
use support::*;

#[test]
fn direct_session_tree_rewind_and_context_isolation() {
    let driver = driver();
    let (world, spec) = direct_world("direct-tree-isolation", "isolation", "");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(&world, "isolation", SseScript::plain("answer"));
    let mut evidence = evidence(&world, &spec);
    let a = SessionSendDriver::new(&driver, &world, &spec, "isolation");
    let b = SessionSendDriver::new(&driver, &world, &spec, "isolation");
    let a1 = a.send("a1").unwrap();
    let a2 = a.send("a2").unwrap();
    let b1 = b.send("b1").unwrap();
    let b2 = b.send("b2").unwrap();
    for (label, turn) in [
        ("会话 A 第 1 轮发送", &a1),
        ("会话 A 第 2 轮发送", &a2),
        ("会话 B 第 1 轮发送", &b1),
        ("会话 B 第 2 轮发送", &b2),
    ] {
        evidence.record_command(label, &turn.command);
    }
    let sid_a = a.session_id().unwrap();
    let sid_b = b.session_id().unwrap();
    let result = (|| {
        for turn in [&a1, &a2, &b1, &b2] {
            assert_clean_send(turn)?;
            require(turn.has_completed(), "初始铺垫轮次应当正常完成")?;
        }
        require(sid_a != sid_b, "两个会话应具有独立的会话标识")?;
        let mut read = |args: &[&str]| -> Value {
            let out = cli(&world, &spec, args);
            evidence.record_command(format!("执行会话命令（{}）", args.join(" ")), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
            serde_json::from_slice(&out.stdout).unwrap()
        };
        let before_a = read(&["--json", "session", "log", sid_a]);
        let before_b = read(&["--json", "session", "log", sid_b]);
        require(
            before_a["nodes"].as_array().unwrap().len() == 5
                && before_b["nodes"].as_array().unwrap().len() == 5,
            "每棵会话树都应包含根节点与两轮对话",
        )?;
        let a_detail = read(&["--json", "session", "turn", a2.accepted_turn_id().unwrap()]);
        let moved = read(&["--json", "session", "rewind", sid_a, "2"]);
        require(moved["current"] == 2, "回退目标应定位到指定节点本身")?;
        require(
            read(&["--json", "session", "log", sid_b]) == before_b,
            "回退会话 A 不得改变会话 B",
        )?;
        let a3 = a.send("a3 fork").unwrap();
        assert_clean_send(&a3)?;
        let after_a = read(&["--json", "session", "log", sid_a]);
        require(
            after_a["nodes"].as_array().unwrap().len() == 7
                && after_a["nodes"][5]["node"]["parent"] == 2
                && after_a["nodes"][5]["node"]["id"] == 5
                && after_a["session"]["current"] == 6,
            "分叉应按最大节点号递增追加，并保留原有分支",
        )?;
        require(
            read(&["--json", "session", "turn", a2.accepted_turn_id().unwrap()]) == a_detail,
            "不在当前主链上的轮次详情应保持不变",
        )?;
        require(
            read(&["--json", "session", "log", sid_b]) == before_b,
            "会话 A 分叉不得改变会话 B",
        )?;
        let invalid = cli(&world, &spec, &["--json", "session", "rewind", sid_b, "6"]);
        require(
            invalid.is_clean_exit(2),
            "仅存在于会话 A 的节点编号不得在会话 B 中解析成功",
        )?;
        require(
            read(&["--json", "session", "log", sid_b]) == before_b,
            "非法的回退请求不得改动会话 B",
        )?;
        let b3 = b.send("b3").unwrap();
        assert_clean_send(&b3)?;
        let captures = server.captured_requests();
        require(
            captures.len() == 6,
            "只读查询与回退操作绝不能触发 provider 请求",
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
            "发往 provider 的上下文应各自沿用本会话从根到当前位点的路径",
        )?;
        evidence.record_command("会话 A 分叉发送", &a3.command);
        evidence.record_command("会话 B 非法回退", &invalid);
        evidence.record_command("会话 B 继续发送", &b3.command);
        Ok(())
    })();
    finish_scenario(evidence, &[], server.captured_requests(), result);
}
