// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 连续观察真实轮次时，中途以 `Continue` 关闭的步骤与带结局的最后一步都交付到条目中，
//! 重试提示作为步骤条目的一部分给出，实时观察与重新打开会话看到的条目一致。
mod support;

#[cfg(feature = "fake-tools")]
use lattice_system_harness::StepEnd;
use lattice_system_harness::{AssertError, SendRecord, SessionSendDriver, SseScript};
use serde_json::Value;
use support::{
    Watcher, assert_accepted_and_closed, assert_clean_send, direct_world, driver, evidence,
    finish_scenario, require, wire_fake_llm,
};
#[cfg(feature = "fake-tools")]
use support::{live_turn, same_turn, sequence, tool_call, tool_ok};

/// `--json` 只输出已知记录类型（含 `configure`），第一条是写明身份的 `submit`。
fn only_seven_records(outcome: &lattice_system_harness::SendOutcome) -> Result<(), AssertError> {
    require(
        !outcome
            .records
            .iter()
            .any(|record| matches!(record, SendRecord::Unknown { .. }))
            && outcome.sending().is_some()
            && matches!(outcome.records.first(), Some(SendRecord::Submit(_))),
        format!(
            "出现了预期之外的记录类型序列：{:?}",
            outcome.type_sequence()
        ),
    )
}

/// 关闭后重新打开会话：新观察者开头给出的条目与 CLI 实时持有的逐字段相同。
/// 只比较条目：`Closed` 已足够让命令结束，占有方在独立收尾后才移除，立即重开时可能仍在场。
fn reopened_entries_match(
    world: &lattice_system_harness::ScenarioWorld,
    spec: &lattice_system_harness::ScenarioSpec,
    outcome: &lattice_system_harness::SendOutcome,
) -> Result<(), AssertError> {
    let (sid, _, _) = outcome.closed_result().expect("轮次应已有收束结果");
    let mut watcher = Watcher::open(world, spec.step_timeout(), sid);
    let reopened = watcher.initial();
    let live: Vec<&Value> = outcome.entries().iter().map(|entry| &entry.raw).collect();
    require(
        reopened.raw_entries() == live,
        "重开会话读到的条目（含重试提示）必须与实时流完全一致",
    )
}

#[cfg(feature = "fake-tools")]
#[test]
fn direct_turn_end_events_completed_turn_delivers_terminal_event() {
    let driver = driver();
    let (world, spec) = direct_world("direct-turn-end-events-completed", "end-events", "");
    world.set_env("LATTICE_TEST_TOOLS", "read_note");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    let server = wire_fake_llm(
        &world,
        "end-events",
        sequence(vec![
            tool_call("observe1", "read_note", r#"{"action":"read"}"#),
            SseScript::plain("workspace observed"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "end-events");
    let outcome = send
        .send("observe the workspace then answer")
        .expect("执行轮次发送");
    let detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap_or_default(),
        ])
        .expect("查询轮次详情");
    let captures = server.captured_requests();
    println!("{}: {:?}", spec.name, outcome.type_sequence());

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        only_seven_records(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(
            outcome.command.is_clean_exit(0),
            "正常完成的轮次退出码应为 0",
        )?;
        require(captures.len() == 2, "应当恰好发起两次 provider 请求")?;
        require(tool_ok(&outcome, "read_note"), "假工具调用成功")?;
        // 第一步以 `Continue` 关闭后不再带结局；结局只随最后一步给出。
        let entries = outcome.entries();
        let steps: Vec<_> = entries.iter().filter_map(|entry| entry.step()).collect();
        require(
            entries.len() == 3
                && entries[0].user_ordinal() == Some(1)
                && steps.len() == 2
                && steps[0].progress
                    == lattice_system_harness::StepProgress::Closed(StepEnd::Continue)
                && steps[0].summary.is_none()
                && steps[0].slots.len() == 1
                && steps[1].ordinal == 1
                && steps[1]
                    .summary
                    .as_ref()
                    .is_some_and(|summary| summary.outcome.as_str() == "completed"),
            format!("应依次包含用户条目、继续执行的步骤与带结局摘要的末步：{entries:?}"),
        )?;
        require(
            detail.is_clean_exit(0)
                && same_turn(
                    &live_turn(&outcome),
                    &serde_json::from_slice::<Value>(&detail.stdout).unwrap(),
                ),
            "实时步骤条目应与重新加载的轮次详情一致",
        )?;
        reopened_entries_match(&world, &spec, &outcome)
    })();
    finish_scenario(
        evidence,
        &[
            ("两步轮次发送", &outcome.command),
            ("查询轮次详情", &detail),
        ],
        captures,
        result,
    );
}

#[test]
fn direct_turn_end_events_exhausted_retries_deliver_last_retry_and_turn_failed() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-turn-end-events-exhausted-retries",
        "end-errors",
        "[llm.retry]\nmax_retries = 2\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(
        &world,
        "end-errors",
        SseScript::HttpError {
            status: 500,
            body: r#"{"error":{"message":"provider down"}}"#.into(),
        },
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "end-errors");
    let outcome = send.send("exhaust provider retries").expect("执行轮次发送");
    let captures = server.captured_requests();
    println!("{}: {:?}", spec.name, outcome.type_sequence());

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        only_seven_records(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(outcome.command.is_clean_exit(1), "失败轮次的退出码应为 1")?;
        require(captures.len() == 3, "应当恰好收到三次 HTTP 500 请求")?;
        let entries = outcome.entries();
        let last = entries
            .iter()
            .rev()
            .find(|entry| entry.step().is_some())
            .expect("应存在最终步骤条目");
        let step = last.step().unwrap();
        let attempts = last.raw["step"]["attempts"].as_array().unwrap();
        // 前两次尝试安排过重试，提示随步骤条目给出，不再有单独的事件。
        require(
            step.retries.iter().map(|r| r.attempt).collect::<Vec<_>>() == [0, 1]
                && attempts.len() == 3,
            format!("应包含第 0、1 次尝试的重试提示：{:?}", step.retries),
        )?;
        let summary = step.summary.as_ref().expect("最终步骤应带有摘要");
        require(
            summary.outcome.as_str() == "failed"
                && summary.notice.as_ref().is_some_and(|notice| {
                    notice.level == "error" && attempts[2]["end"]["diagnostic"] == notice.text
                }),
            "终态提示文本必须与最后一次尝试的诊断信息一致",
        )?;
        reopened_entries_match(&world, &spec, &outcome)
    })();
    finish_scenario(
        evidence,
        &[("重试耗尽轮次发送", &outcome.command)],
        captures,
        result,
    );
}
