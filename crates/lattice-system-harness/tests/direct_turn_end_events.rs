// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 连续观察真实 Turn 时，中途以 `Continue` 关闭的 Step 与带结局的最后一步都交付到条目中，
//! 重试提示是 Step 条目的一部分，实时观察与重新打开会话看到的条目相同。
mod support;

use lattice_system_harness::{AssertError, SendRecord, SessionSendDriver, SseScript, StepEnd};
use serde_json::Value;
use support::{
    Watcher, assert_accepted_and_closed, assert_clean_send, direct_world, driver, evidence,
    finish_scenario, live_turn, require, same_turn, sequence, tool_call, tool_ok, wire_fake_llm,
};

/// `--json` 只有七种记录，第一条是写明身份的 `submit`。
fn only_seven_records(outcome: &lattice_system_harness::SendOutcome) -> Result<(), AssertError> {
    require(
        !outcome
            .records
            .iter()
            .any(|record| matches!(record, SendRecord::Unknown { .. }))
            && outcome.sending().is_some()
            && matches!(outcome.records.first(), Some(SendRecord::Submit(_))),
        format!("unexpected record types: {:?}", outcome.type_sequence()),
    )
}

/// 关闭后重新打开会话：新观察者开头给出的条目与 CLI 实时持有的逐字段相同。
/// 只比较条目：`Closed` 已足够让命令结束，Owner 在独立收尾后才移除，立即重开可能仍有 Owner。
fn reopened_entries_match(
    world: &lattice_system_harness::ScenarioWorld,
    spec: &lattice_system_harness::ScenarioSpec,
    outcome: &lattice_system_harness::SendOutcome,
) -> Result<(), AssertError> {
    let (sid, _, _) = outcome.closed_result().expect("closed result");
    let mut watcher = Watcher::open(world, spec.step_timeout(), sid);
    let reopened = watcher.initial();
    let live: Vec<&Value> = outcome.entries().iter().map(|entry| &entry.raw).collect();
    require(
        reopened.raw_entries() == live,
        "reopened entries must equal the live ones, retry notices included",
    )
}

#[test]
fn direct_turn_end_events_completed_turn_delivers_terminal_event() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-turn-end-events-completed",
        "end-events",
        "[\"observe.workspace\"]",
        "",
    );
    let server = wire_fake_llm(
        &world,
        "end-events",
        sequence(vec![
            tool_call(
                "observe1",
                "observe.workspace",
                r#"{"request":{"mode":"list_directory","path":"."}}"#,
            ),
            SseScript::plain("workspace observed"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "end-events");
    let outcome = send
        .send("observe the workspace then answer")
        .expect("turn");
    let detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap_or_default(),
        ])
        .expect("turn detail");
    let captures = server.captured_requests();
    println!("{}: {:?}", spec.name, outcome.type_sequence());

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        only_seven_records(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(outcome.command.is_clean_exit(0), "completed Turn exits 0")?;
        require(captures.len() == 2, "exactly two provider requests")?;
        require(
            tool_ok(&outcome, "observe.workspace"),
            "observation succeeds",
        )?;
        // 第一步以 Continue 关闭后不再带结局；结局只随最后一步给出。
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
            format!("expected user, continued step and final step with summary: {entries:?}"),
        )?;
        require(
            detail.is_clean_exit(0)
                && same_turn(
                    &live_turn(&outcome),
                    &serde_json::from_slice::<Value>(&detail.stdout).unwrap(),
                ),
            "live Step entries equal the reloaded Turn detail",
        )?;
        reopened_entries_match(&world, &spec, &outcome)
    })();
    finish_scenario(
        evidence,
        &[
            ("two-step Turn", &outcome.command),
            ("turn detail", &detail),
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
        "[]",
        "[llm.retry]\nmax_retries = 2\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
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
    let outcome = send.send("exhaust provider retries").expect("turn");
    let captures = server.captured_requests();
    println!("{}: {:?}", spec.name, outcome.type_sequence());

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        only_seven_records(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(outcome.command.is_clean_exit(1), "failed Turn exits 1")?;
        require(captures.len() == 3, "exactly three HTTP 500 requests")?;
        let entries = outcome.entries();
        let last = entries
            .iter()
            .rev()
            .find(|entry| entry.step().is_some())
            .expect("final Step entry");
        let step = last.step().unwrap();
        let attempts = last.raw["step"]["attempts"].as_array().unwrap();
        // 前两次尝试安排过重试，提示随 Step 条目给出，不再有单独的事件。
        require(
            step.retries.iter().map(|r| r.attempt).collect::<Vec<_>>() == [0, 1]
                && attempts.len() == 3,
            format!(
                "expected retry notices for attempts [0, 1]: {:?}",
                step.retries
            ),
        )?;
        let summary = step.summary.as_ref().expect("final step summary");
        require(
            summary.outcome.as_str() == "failed"
                && summary.notice.as_ref().is_some_and(|notice| {
                    notice.level == "error" && attempts[2]["end"]["diagnostic"] == notice.text
                }),
            "terminal notice must equal the final attempt diagnostic",
        )?;
        reopened_entries_match(&world, &spec, &outcome)
    })();
    finish_scenario(
        evidence,
        &[("exhausted retries", &outcome.command)],
        captures,
        result,
    );
}
