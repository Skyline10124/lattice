// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local atomic mutation, external concurrency, and effect denial
//! (issue #46 slice 3, Codeberg #24).
//!
//! Covers `mutate__create` / `mutate__edit` operating on current Local state:
//! success paths leave bytes immediately; denials and stale preconditions
//! never change the target; preflight effect-kind denial never calls the backend.
//! Mutation retention after provider failure is covered by `direct_failures`.
//!
//! Tool results are inspected in the next captured provider request (the
//! product feeds the CEP result back as a `tool` role message).

mod support;

use std::fs;
use std::time::Duration;

use lattice_system_harness::{
    AssertError, SendExitCode, SessionSendDriver, SlotOutcome, SseScript,
};
use serde_json::Value;
use support::{
    assert_accepted_and_closed, assert_clean_send, assert_completed_after_tools, direct_world,
    driver, evidence, finish_scenario, finished_with_code, require, sequence, tool_call,
    tool_failed, tool_failure, tool_ok, tool_result_value, wire_fake_llm, wire_gated_fake_llm,
};

fn is_non_completed_terminal(outcome: &lattice_system_harness::SendOutcome) -> bool {
    outcome
        .closed_result()
        .is_some_and(|(_, _, outcome)| outcome != "completed")
        || outcome.exit_code == Some(SendExitCode::Failed)
}

/// `mutate__create` succeeds only while the target is absent; bytes appear
/// immediately. A second create of the same path fails and preserves bytes.
#[test]
fn direct_mutation_create_atomic_and_existing_target() {
    let driver = driver();
    let (world, spec) = direct_world("direct-mut-create", "dl-create", "[\"mutate__create\"]", "");
    let server = wire_fake_llm(
        &world,
        "dl-create",
        sequence(vec![
            tool_call(
                "c1",
                "mutate__create",
                r#"{"path":"new.txt","content":"created-v1\n","create_parents":false}"#,
            ),
            SseScript::plain("created"),
            tool_call(
                "c2",
                "mutate__create",
                r#"{"path":"new.txt","content":"created-v2\n","create_parents":false}"#,
            ),
            SseScript::plain("second create failed as expected"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-create");

    let first = send.send("create new.txt").expect("first turn");
    let second = send.send("create new.txt again").expect("second turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        require(tool_ok(&first.parsed, "mutate__create"), "first create ok")?;
        require(
            first.has_completed(),
            format!("first turn incomplete: {:?}", first.type_sequence()),
        )?;
        let path = world.project_dir().join("new.txt");
        require(path.is_file(), "create did not write new.txt")?;
        require(
            fs::read_to_string(&path).unwrap_or_default() == "created-v1\n",
            format!("create bytes wrong: {:?}", fs::read(&path)),
        )?;

        require(
            tool_failed(&second.parsed, "mutate__create"),
            "second create of existing target must fail",
        )?;
        require(
            second.has_completed(),
            "second turn should still complete after tool failure",
        )?;
        require(
            fs::read_to_string(&path).unwrap_or_default() == "created-v1\n",
            "failed create must preserve existing bytes",
        )?;

        let failure = tool_failure(&second.parsed, "mutate__create");
        require(
            failure.is_some_and(|(_, error)| {
                error["code"] == "replan_required"
                    && error["data"]["kind"] == "guard"
                    && error["data"]["reason"] == "ReplanRequired: already-exists"
            }),
            format!("existing-target reason required: {failure:?}"),
        )?;

        // Successful create result JSON in next provider request.
        require(captures.len() >= 2, "expected create + recovery captures")?;
        let created = tool_result_value(&captures[1]).expect("create result JSON");
        require(
            created.get("path").and_then(Value::as_str) == Some("new.txt")
                || created.pointer("/result/path").and_then(Value::as_str) == Some("new.txt")
                || created.to_string().contains("new.txt"),
            format!("create result missing path: {created}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("create success", &first.command),
            ("create existing fails", &second.command),
        ],
        captures,
        result,
    );
}

/// `mutate__edit` with a known Local Revision succeeds; a nonmatching anchor
/// fails before install and preserves bytes.
#[test]
fn direct_mutation_edit_hash_and_anchor() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-mut-edit",
        "dl-edit",
        "[\"observe__workspace\", \"mutate__edit\"]",
        "",
    );
    world
        .write_project_file("tracked.txt", "hello world\n")
        .expect("seed file");
    world.commit_all("seed tracked").expect("commit seed");

    let server = wire_fake_llm(
        &world,
        "dl-edit",
        sequence(vec![
            // Round 1: observe to learn content_hash
            tool_call(
                "o1",
                "observe__workspace",
                r#"{"request":{"mode":"read_file","path":"tracked.txt"}}"#,
            ),
            // Round 2: edit with the hash from the previous tool message —
            // but the fake LLM cannot dynamically read captures. We drive
            // two separate turns instead: observe turn, then edit turn with
            // hash extracted from captures.
            SseScript::plain("observed"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-edit");

    let observe = send.send("read tracked.txt").expect("observe turn");
    let captures_obs = server.captured_requests();
    let after_observe =
        fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default();
    assert_eq!(
        after_observe, "hello world\n",
        "file changed during observe: {after_observe:?}"
    );

    // Shut down first server; we'll spin a second one for edit with the hash.
    drop(server);

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&observe)?;
        require(tool_ok(&observe.parsed, "observe__workspace"), "observe ok")?;
        require(captures_obs.len() >= 2, "observe needs tool result capture")?;
        let observed = tool_result_value(&captures_obs[1]).expect("observe result");
        let hash = observed
            .pointer("/result/content_hash")
            .and_then(Value::as_str)
            .or_else(|| observed.get("content_hash").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        require(
            hash.starts_with("blake3:") && hash.len() == "blake3:".len() + 64,
            format!("bad content_hash from observe: {observed}"),
        )?;

        let good_args = format!(
            r#"{{"path":"tracked.txt","expected_hash":"{hash}","edits":[{{"op":"replace_text","old":"hello world","new":"hello lattice"}}]}}"#
        );
        let bad_args = format!(
            r#"{{"path":"tracked.txt","expected_hash":"{hash}","edits":[{{"op":"replace_text","old":"NO_SUCH_ANCHOR","new":"x"}}]}}"#
        );

        // After good edit the hash changes; for the bad-anchor case we need
        // the post-good hash OR we run bad-anchor first on the original file.
        // Order: bad-anchor first (preserves bytes + original hash), then good edit.
        let server2 = wire_fake_llm(
            &world,
            "dl-edit",
            sequence(vec![
                tool_call("e_bad", "mutate__edit", &bad_args),
                SseScript::plain("bad anchor denied"),
                tool_call("e_good", "mutate__edit", &good_args),
                SseScript::plain("edited"),
            ]),
        );

        let bad = send.send("edit with bad anchor").expect("bad edit turn");
        assert_clean_send(&bad)?;
        require(
            tool_failed(&bad.parsed, "mutate__edit"),
            format!(
                "nonmatching anchor must fail; events={:?}",
                bad.type_sequence()
            ),
        )?;
        require(
            fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default()
                == "hello world\n",
            "failed edit must preserve original bytes",
        )?;
        require(bad.has_completed(), "bad-edit turn completes")?;
        let failure = tool_failure(&bad.parsed, "mutate__edit");
        require(
            failure.is_some_and(|(_, error)| {
                error["code"] == "replan_required"
                    && error["data"]["kind"] == "guard"
                    && error["data"]["reason"] == "ReplanRequired: semantic-replan"
            }),
            format!("anchor replan reason required: {failure:?}"),
        )?;

        let good = send.send("edit with good anchor").expect("good edit turn");
        let captures_edit = server2.captured_requests();

        assert_clean_send(&good)?;
        require(tool_ok(&good.parsed, "mutate__edit"), "good edit ok")?;
        require(
            fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default()
                == "hello lattice\n",
            "good edit must install new bytes",
        )?;
        require(good.has_completed(), "good-edit turn completes")?;
        require(
            captures_edit.len() >= 2,
            format!("edit captures missing: {}", captures_edit.len()),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("observe for hash", &observe.command)],
        captures_obs,
        result,
    );
}

/// External writer changes the file between observe and edit; stale edit
/// returns failure and preserves external bytes. External writes between
/// Tool calls are visible to the next observe.
#[test]
fn direct_mutation_stale_edit_and_external_visibility() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-mut-stale",
        "dl-stale",
        "[\"observe__workspace\", \"mutate__edit\"]",
        "",
    );
    world
        .write_project_file("tracked.txt", "original\n")
        .expect("seed");
    world
        .write_project_file("orig.txt", "x\n")
        .expect("seed list entry");
    world.commit_all("seed").expect("commit");

    let (server, gate) = wire_gated_fake_llm(
        &world,
        "dl-stale",
        sequence(vec![
            tool_call(
                "o1",
                "observe__workspace",
                r#"{"request":{"mode":"read_file","path":"tracked.txt"}}"#,
            ),
            tool_call(
                "list1",
                "observe__workspace",
                r#"{"request":{"mode":"list_directory","path":"."}}"#,
            ),
            tool_call(
                "list2",
                "observe__workspace",
                r#"{"request":{"mode":"list_directory","path":"."}}"#,
            ),
            SseScript::plain("observed original and external addition"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-stale");
    let running = send
        .spawn("read tracked and list twice")
        .expect("spawn observe");
    for count in 1..=2 {
        server
            .wait_for_requests(count, running.remaining_timeout(Duration::from_secs(30)))
            .expect("observe request");
        gate.wait_until_stalled(count, running.remaining_timeout(Duration::from_secs(2)))
            .expect("observe response stalled");
        gate.release_next(running.remaining_timeout(Duration::from_secs(2)))
            .expect("release read or first list");
    }
    server
        .wait_for_requests(3, running.remaining_timeout(Duration::from_secs(30)))
        .expect("first list result reached provider");
    gate.wait_until_stalled(3, running.remaining_timeout(Duration::from_secs(2)))
        .expect("second list stalled before execution");
    world
        .write_project_file("added.txt", "new\n")
        .expect("external addition");
    gate.release(running.remaining_timeout(Duration::from_secs(2)))
        .expect("release second list");
    let observe = send.wait_spawned(running).expect("observe");
    let captures_obs = server.captured_requests();
    drop(server);

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&observe)?;
        let first_list = tool_result_value(&captures_obs[2]).expect("first list result");
        let second_list = tool_result_value(&captures_obs[3]).expect("second list result");
        for (label, list) in [("first", &first_list), ("second", &second_list)] {
            require(
                list["mode"] == "list_directory",
                format!("{label} list must retain list_directory mode: {list}"),
            )?;
            require(
                list["result"]["entries"]
                    .as_array()
                    .is_some_and(|entries| entries.iter().all(|entry| entry["name"].is_string())),
                format!("{label} list must return named entries: {list}"),
            )?;
        }
        let first_entries = first_list["result"]["entries"]
            .as_array()
            .expect("first entries");
        require(
            first_entries
                .iter()
                .any(|entry| entry["name"] == "orig.txt"),
            format!("first list must contain orig.txt: {first_list}"),
        )?;
        require(
            !first_entries
                .iter()
                .any(|entry| entry["name"] == "added.txt"),
            format!("first list must not contain added.txt: {first_list}"),
        )?;
        require(
            second_list["result"]["entries"]
                .as_array()
                .expect("second entries")
                .iter()
                .any(|entry| entry["name"] == "added.txt"),
            format!("second list must see externally added added.txt: {second_list}"),
        )?;
        let observed = tool_result_value(&captures_obs[1]).expect("observe result");
        let hash = observed
            .pointer("/result/content_hash")
            .and_then(Value::as_str)
            .or_else(|| observed.get("content_hash").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        require(
            hash.starts_with("blake3:"),
            format!("hash missing: {observed}"),
        )?;

        world
            .write_project_file("tracked.txt", "external\n")
            .expect("external write");

        let stale_args = format!(
            r#"{{"path":"tracked.txt","expected_hash":"{hash}","edits":[{{"op":"replace_text","old":"original","new":"agent"}}]}}"#
        );
        let server2 = wire_fake_llm(
            &world,
            "dl-stale",
            sequence(vec![
                tool_call("e_stale", "mutate__edit", &stale_args),
                SseScript::plain("stale denied"),
                tool_call(
                    "o2",
                    "observe__workspace",
                    r#"{"request":{"mode":"read_file","path":"tracked.txt"}}"#,
                ),
                SseScript::plain("see external"),
            ]),
        );

        let stale = send.send("edit with stale hash").expect("stale edit");
        assert_clean_send(&stale)?;
        require(
            tool_failed(&stale.parsed, "mutate__edit"),
            "stale edit must fail",
        )?;
        // Host `StaleRevision` records a typed refusal; runtime prefers it over
        // the guest's `Err(String)` retelling so the wire stays ReplanRequired.
        require(
            finished_with_code(&stale.parsed, "mutate__edit", "replan_required"),
            format!(
                "stale edit must emit ReplanRequired: {:?}",
                stale.parsed.slots()
            ),
        )?;
        require(
            fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default()
                == "external\n",
            "stale edit must preserve external bytes",
        )?;

        let reobserve = send.send("read after external").expect("reobserve");
        let captures2 = server2.captured_requests();
        assert_clean_send(&reobserve)?;
        require(
            tool_ok(&reobserve.parsed, "observe__workspace"),
            "reobserve ok",
        )?;
        let base = captures2.len().saturating_sub(2);
        let seen = tool_result_value(&captures2[base + 1]).expect("reobserve result");
        let content = seen
            .pointer("/result/content")
            .and_then(Value::as_str)
            .or_else(|| seen.get("content").and_then(Value::as_str))
            .unwrap_or("");
        require(
            content.contains("external"),
            format!("reobserve must see external bytes: {seen}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("observe original", &observe.command)],
        captures_obs,
        result,
    );
}

/// Write policy, system exclusion, and Cell capability denial produce no
/// filesystem side effect.
#[test]
fn direct_mutation_denials_have_no_side_effect() {
    let driver = driver();

    let (world, spec) = direct_world(
        "direct-mut-deny-write",
        "dl-dwrite",
        "[\"mutate__create\"]",
        "\n[lancet.workspace]\ndenied_write_patterns = [\"blocked/**\"]\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-dwrite",
        sequence(vec![
            tool_call(
                "c1",
                "mutate__create",
                r#"{"path":"blocked/x.txt","content":"nope","create_parents":true}"#,
            ),
            SseScript::plain("denied"),
        ]),
    );
    let mut scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-dwrite");
    let write_deny = send.send("write denied").expect("turn");
    let caps1 = server.captured_requests();
    drop(server);

    let (world2, spec2) =
        direct_world("direct-mut-deny-sys", "dl-dsys", "[\"mutate__create\"]", "");
    let server2 = wire_fake_llm(
        &world2,
        "dl-dsys",
        sequence(vec![
            tool_call(
                "c2",
                "mutate__create",
                r#"{"path":".lattice/evil.txt","content":"nope","create_parents":true}"#,
            ),
            SseScript::plain("denied"),
        ]),
    );
    let mut evidence2 = evidence(&world2, &spec2);
    let send2 = SessionSendDriver::new(&driver, &world2, &spec2, "dl-dsys");
    let sys_deny = send2.send("write .lattice").expect("turn");
    let caps2 = server2.captured_requests();
    drop(server2);

    // Capability denial must refuse before the Component reaches installation.
    let (world3, spec3) = direct_world(
        "direct-mut-deny-effect",
        "dl-deff",
        "[\"mutate__create\"]",
        "\n[lancet.capabilities]\ndeny = [\"workspace.mutation\"]\n",
    );
    let server3 = wire_fake_llm(
        &world3,
        "dl-deff",
        sequence(vec![
            tool_call(
                "c3",
                "mutate__create",
                r#"{"path":"effect-denied.txt","content":"nope","create_parents":false}"#,
            ),
            SseScript::plain("effect denied"),
        ]),
    );
    let mut evidence3 = evidence(&world3, &spec3);
    let send3 = SessionSendDriver::new(&driver, &world3, &spec3, "dl-deff");
    let effect_deny = send3.send("effect denied create").expect("turn");
    let caps3 = server3.captured_requests();

    let write_result: Result<(), AssertError> = (|| {
        assert_clean_send(&write_deny)?;
        require(
            tool_failed(&write_deny.parsed, "mutate__create"),
            "write policy must fail tool",
        )?;
        require(
            !world.project_dir().join("blocked/x.txt").exists(),
            "write-policy denial left a file",
        )?;
        require(
            finished_with_code(&write_deny.parsed, "mutate__create", "capability_violation"),
            format!(
                "write-policy denial must be CapabilityViolation: {:?}",
                write_deny.parsed.slots()
            ),
        )?;

        let failure = tool_failure(&write_deny.parsed, "mutate__create");
        require(
            failure.is_some_and(|(slot, error)| {
                error["code"] == "capability_violation"
                    && error["data"]["kind"] == "capability"
                    && error["data"]["capability"] == "mutate:write:workspace"
                    && error["data"]["violations"] == serde_json::json!(["denied"])
                    && slot["progress"]["evidence"]["value"]["admission"]["denied_capabilities"]
                        == serde_json::json!([])
                    && slot["progress"]["evidence"]["value"]["admission"]["approved_capabilities"]
                        .as_array()
                        .is_some_and(|caps| caps.iter().any(|cap| cap == "workspace.mutation"))
            }),
            format!(
                "typed write-policy source with admitted workspace.mutation required: {failure:?}"
            ),
        )?;
        require(
            caps1.len() == 2,
            "write denial requires tool + recovery requests",
        )?;
        Ok(())
    })();
    let sys_result: Result<(), AssertError> = (|| {
        assert_clean_send(&sys_deny)?;
        require(
            tool_failed(&sys_deny.parsed, "mutate__create"),
            "system exclusion must fail tool",
        )?;
        require(
            !world2.project_dir().join(".lattice/evil.txt").exists(),
            "system-exclusion denial left a file",
        )?;
        require(
            finished_with_code(&sys_deny.parsed, "mutate__create", "path_containment"),
            format!(
                "system exclusion must be PathContainment: {:?}",
                sys_deny.parsed.slots()
            ),
        )?;

        let failure = tool_failure(&sys_deny.parsed, "mutate__create");
        require(
            failure.is_some_and(|(slot, error)| {
                error["code"] == "path_containment"
                    && error["data"]["kind"] == "validation"
                    && error["data"]["field"] == "path"
                    && error["data"]["reason"] == "path-rejected"
                    && slot["progress"]["evidence"]["value"]["admission"]["denied_capabilities"]
                        == serde_json::json!([])
                    && slot["progress"]["evidence"]["value"]["admission"]["approved_capabilities"]
                        .as_array()
                        .is_some_and(|caps| caps.iter().any(|cap| cap == "workspace.mutation"))
            }),
            format!(
                "typed system-exclusion source with admitted workspace.mutation required: {failure:?}"
            ),
        )?;
        require(
            caps2.len() == 2,
            "system denial requires tool + recovery requests",
        )?;
        Ok(())
    })();
    let effect_result: Result<(), AssertError> = (|| {
        assert_clean_send(&effect_deny)?;
        require(
            tool_failed(&effect_deny.parsed, "mutate__create"),
            "capability denial must fail tool",
        )?;
        require(
            !world3.project_dir().join("effect-denied.txt").exists(),
            "capability denial left a file",
        )?;
        require(
            caps3.len() == 2,
            "capability denial needs tool result capture",
        )?;
        let failure = tool_failure(&effect_deny.parsed, "mutate__create");
        require(
            failure.is_some_and(|(slot, error)| {
                error["code"] == "capability_violation"
                    && error["data"]["kind"] == "capability"
                    && slot["progress"]["evidence"]["value"]["admission"]["denied_capabilities"]
                        == serde_json::json!(["workspace.mutation"])
            }),
            format!("workspace.mutation admission denial required: {failure:?}"),
        )?;
        Ok(())
    })();

    scenario_evidence.record_command("write deny", &write_deny.command);
    scenario_evidence.record_captures(caps1);
    evidence2.record_command("sys deny", &sys_deny.command);
    evidence2.record_captures(caps2);
    evidence3.record_command("capability deny", &effect_deny.command);
    evidence3.record_captures(caps3);
    // 每个 world 都独立扫描并收尾，再汇总报告，避免首个失败跳过其余清理。
    let reports: Vec<_> = [scenario_evidence, evidence2, evidence3]
        .into_iter()
        .zip([write_result, sys_result, effect_result])
        .filter_map(|(evidence, result)| {
            evidence
                .finish_report(result.err().map(|error| error.to_string()))
                .err()
        })
        .collect();
    if !reports.is_empty() {
        panic!(
            "{}",
            reports
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n\n")
        );
    }
}

/// 同一项目的两个 Session 创建同一尚不存在的目标；两个 Turn 都停在 provider 闸口，
/// 共同放行后真实竞争 Tool 执行：恰好一个 Component 调用安装成功，另一方返回
/// `ReplanRequired`。会话提交竞争是另一层问题，不能削弱本地变更的断言。
///
/// 同一 Session 内的并发 send 会被产品以 `Busy` 拒绝，根本进不到 Tool 层，因此竞争必须由
/// 两个不同 Session 发起；gate 要求两个 provider 应答同时停在闸口后再一起放行。
#[test]
fn direct_mutation_concurrent_same_target_create_has_one_winner() {
    let driver = driver();
    let (world, spec) = direct_world("direct-mut-race", "dl-race", "[\"mutate__create\"]", "");

    // Sequence 按请求到达顺序供稿：前两个请求（两个 Session 各一）都拿到同一目标的 create，
    // 后两个是工具回合后的收尾文本。
    let (server, gate) = wire_gated_fake_llm(
        &world,
        "dl-race",
        sequence(vec![
            tool_call(
                "r1",
                "mutate__create",
                r#"{"path":"race.txt","content":"a\n","create_parents":false}"#,
            ),
            tool_call(
                "r2",
                "mutate__create",
                r#"{"path":"race.txt","content":"b\n","create_parents":false}"#,
            ),
            SseScript::plain("a done"),
            SseScript::plain("b done"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send_a = SessionSendDriver::new(&driver, &world, &spec, "dl-race");
    let send_b = SessionSendDriver::new(&driver, &world, &spec, "dl-race");

    let running_a = send_a.spawn("create a").expect("spawn a");
    let running_b = send_b.spawn("create b").expect("spawn b");
    let step = |secs: u64| {
        running_a
            .remaining_timeout(Duration::from_secs(secs))
            .min(running_b.remaining_timeout(Duration::from_secs(secs)))
    };

    server
        .wait_for_requests(2, step(30))
        .expect("both creates reached the fake provider");
    gate.wait_until_stalled(2, step(2))
        .expect("both create handlers reached the response gate together");
    gate.release(step(2))
        .expect("both create handlers left the response gate");

    let a = send_a.wait_spawned(running_a).expect("wait a");
    let b = send_b.wait_spawned(running_b).expect("wait b");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        require(
            send_a.session_id() != send_b.session_id(),
            "race must run in two distinct sessions of the same project",
        )?;
        assert_clean_send(&a)?;
        assert_clean_send(&b)?;
        let tool_successes = [&a, &b]
            .into_iter()
            .filter(|outcome| tool_ok(&outcome.parsed, "mutate__create"))
            .count();
        let tool_failures = [&a, &b]
            .into_iter()
            .filter(|outcome| tool_failed(&outcome.parsed, "mutate__create"))
            .count();
        require(
            tool_successes == 1 && tool_failures == 1,
            format!(
                "same-target create must have exactly one Tool winner and one loser; a={:?} b={:?}",
                a.type_sequence(),
                b.type_sequence()
            ),
        )?;
        // 两个 Tool 执行都真实发生过：各自的槽位都已交给 LANCET，而不是一方被 Busy 拒在门外。
        require(
            [&a, &b].into_iter().all(|outcome| {
                outcome.slots().iter().any(|slot| {
                    slot.method == "mutate__create" && slot.outcome != SlotOutcome::NotExecuted
                })
            }),
            format!(
                "both sessions must execute the Tool; a={:?} b={:?}",
                a.type_sequence(),
                b.type_sequence()
            ),
        )?;
        let loser = if tool_failed(&a.parsed, "mutate__create") {
            &a
        } else {
            &b
        };
        require(
            finished_with_code(&loser.parsed, "mutate__create", "replan_required"),
            format!(
                "same-target loser must be ReplanRequired: {:?}",
                loser.type_sequence()
            ),
        )?;

        let a_completed = a.has_completed();
        let b_completed = b.has_completed();
        require(
            a_completed || b_completed,
            format!(
                "expected at least one commit winner; a={:?} b={:?}",
                a.type_sequence(),
                b.type_sequence()
            ),
        )?;
        if a_completed && !b_completed {
            require(
                is_non_completed_terminal(&b.parsed),
                format!(
                    "loser b must fail deterministically: {:?}",
                    b.type_sequence()
                ),
            )?;
        }
        if b_completed && !a_completed {
            require(
                is_non_completed_terminal(&a.parsed),
                format!(
                    "loser a must fail deterministically: {:?}",
                    a.type_sequence()
                ),
            )?;
        }
        let race_path = world.project_dir().join("race.txt");
        require(
            race_path.is_file(),
            "successful mutate__create must leave race.txt",
        )?;
        let bytes = fs::read_to_string(&race_path).unwrap_or_default();
        require(
            bytes == "a\n" || bytes == "b\n",
            format!("race winner left unexpected bytes: {bytes:?}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("race a", &a.command), ("race b", &b.command)],
        captures,
        result,
    );
}

/// Lifecycle: tool activity precedes commit on a successful mutation turn.
#[test]
fn direct_mutation_tool_activity_before_commit() {
    let driver = driver();
    let (world, spec) = direct_world("direct-mut-order", "dl-order", "[\"mutate__create\"]", "");
    let server = wire_fake_llm(
        &world,
        "dl-order",
        sequence(vec![
            tool_call(
                "c1",
                "mutate__create",
                r#"{"path":"ordered.txt","content":"ok\n","create_parents":false}"#,
            ),
            SseScript::plain("done"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-order");
    let outcome = send.send("create ordered").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        // 结局来自 Step 条目：接受回执与同一轮的结局对应，
        // 带结局的最后一步在带 Tool 调用的步骤之后。
        assert_accepted_and_closed(&outcome.parsed)?;
        assert_completed_after_tools(&outcome.parsed)?;
        require(tool_ok(&outcome.parsed, "mutate__create"), "create ok")?;
        require(
            world.project_dir().join("ordered.txt").is_file(),
            "ordered.txt missing",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("ordered create", &outcome.command)],
        captures,
        result,
    );
}

/// 指向外部目录的符号链接不能成为 `mutate__create` 的父目录。
#[test]
fn direct_mutation_symlink_parent_is_refused_without_side_effect() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-mut-symlink",
        "dl-symlink",
        "[\"mutate__create\"]",
        "",
    );
    const CANARY: &str = "I233_MUTATION_EXTERNAL_CANARY_83bc\n";
    let outside = world.root().join("outside-mutation");
    fs::create_dir(&outside).expect("outside directory");
    let canary_file = outside.join("canary.txt");
    fs::write(&canary_file, CANARY).expect("outside canary");
    std::os::unix::fs::symlink(&outside, world.project_dir().join("dir-link"))
        .expect("directory symlink");
    let server = wire_fake_llm(
        &world,
        "dl-symlink",
        sequence(vec![
            tool_call(
                "symlink-create",
                "mutate__create",
                r#"{"path":"dir-link/new.txt","content":"nope","create_parents":true}"#,
            ),
            SseScript::plain("symlink refused"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let execution = SessionSendDriver::new(&driver, &world, &spec, "dl-symlink")
        .send("create through symlink")
        .expect("turn drives");
    let captures = server.captured_requests();
    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&execution)?;
        require(
            tool_failed(&execution.parsed, "mutate__create"),
            "symlink create must fail",
        )?;
        require(
            finished_with_code(&execution.parsed, "mutate__create", "path_containment"),
            "symlink create must be PathContainment",
        )?;
        require(
            captures.len() == 2,
            "expected symlink create result in provider capture",
        )?;
        let error = tool_result_value(&captures[1]).expect("symlink create error envelope");
        require(
            error.pointer("/error/code").and_then(Value::as_i64) == Some(-32002),
            format!("symlink create error code mismatch: {error}"),
        )?;
        require(
            captures
                .iter()
                .all(|capture| !capture.raw.to_string().contains(CANARY.trim())),
            "outside canary leaked into provider requests",
        )?;
        require(
            !outside.join("new.txt").exists()
                && fs::read_to_string(&canary_file).expect("read canary") == CANARY
                && fs::read_dir(&outside).expect("outside entries").count() == 1,
            "outside directory changed during create denial",
        )?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[("symlink create", &execution.command)],
        captures,
        result,
    );
}
