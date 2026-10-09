// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 连续观察真实 Turn 时，交付中途 Step 关闭和最终结束事件（#224）。
mod support;

use lattice_system_harness::{AssertError, SessionSendDriver, SseScript, TurnEventLine};
use support::{
    assert_accepted_and_closed, assert_clean_send, direct_world, driver, evidence, finish_scenario,
    require, sequence, tool_call, tool_ok, wire_fake_llm,
};

fn is_summary_snapshot(event: &TurnEventLine) -> bool {
    matches!(event, TurnEventLine::Unknown { type_name, raw }
        if type_name == "snapshot" && raw["view"]["selected_turn"]["summary"].is_object())
}

fn event_sequence(events: &[TurnEventLine]) -> String {
    let mut types = Vec::new();
    let mut index = 0;
    while index < events.len() {
        if matches!(events[index], TurnEventLine::TokenDelta { .. }) {
            let count = events[index..]
                .iter()
                .take_while(|event| matches!(event, TurnEventLine::TokenDelta { .. }))
                .count();
            types.push(format!("token_delta×{count}"));
            index += count;
            continue;
        }
        types.push(if is_summary_snapshot(&events[index]) {
            "snapshot(summary)".into()
        } else if let TurnEventLine::LlmRetry { key, .. } = &events[index] {
            format!("llm_retry(attempt={})", key["attempt_ordinal"])
        } else {
            events[index].type_name().into()
        });
        index += 1;
    }
    format!("[{}]", types.join(", "))
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
    let captures = server.captured_requests();
    let got = event_sequence(&outcome.events);
    println!("{}: {got}", spec.name);

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(outcome.command.is_clean_exit(0), "completed Turn exits 0")?;
        require(captures.len() == 2, "exactly two provider requests")?;
        require(
            tool_ok(&outcome, "observe.workspace"),
            "observation succeeds",
        )?;
        let starts: Vec<_> = outcome
            .events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| {
                matches!(event, TurnEventLine::ModelStarted { .. }).then_some(index)
            })
            .collect();
        let finishes: Vec<_> = outcome
            .events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| {
                matches!(event, TurnEventLine::ModelFinished { .. }).then_some(index)
            })
            .collect();
        // 订阅与执行并行（session.md「观察与重连」）：快照之前发出的事件不重发，
        // 订阅晚于第一步时收不到它的 model_*。只在实际收到两步时核验中途 step_closed。
        require(
            starts.len() <= 2 && finishes.len() <= 2,
            format!("expected at most two model_started/model_finished; got: {got}"),
        )?;
        if starts.len() == 2 && finishes.len() == 2 {
            require(
                outcome.events.iter().enumerate().any(|(index, event)| {
                    matches!(event, TurnEventLine::Unknown { type_name, raw }
                        if type_name == "step_closed" && raw["step"]["key"]["ordinal"] == 0)
                        && finishes[0] < index
                        && index < starts[1]
                }),
                format!("expected first-step step_closed before second model_started; got: {got}"),
            )?;
            println!("{}: control step_closed(ordinal=0) delivered", spec.name);
        }

        // 只核验实际 Terminal；summary 快照不能替代结束事件。
        let terminal = outcome.events.iter().position(|event| {
            matches!(event, TurnEventLine::Terminal { type_name, .. } if type_name == "turn_completed")
        });
        require(
            terminal.is_some_and(|index| {
                finishes.last().is_none_or(|last| *last < index)
                    && outcome
                        .events
                        .iter()
                        .enumerate()
                        .all(|(snapshot, event)| !is_summary_snapshot(event) || index < snapshot)
            }),
            format!(
                "expected turn_completed after last model_finished and before any snapshot(summary); got: {got}"
            ),
        )?;
        let terminal = terminal.unwrap();
        require(
            matches!(&outcome.events[terminal], TurnEventLine::Terminal { raw, .. }
                if raw["step"]["key"]["ordinal"] == 1),
            format!("expected turn_completed with second-step ordinal=1; got: {got}"),
        )?;
        require(
            outcome.events[terminal + 1..].iter().all(|event| {
                event.type_name() != "token_delta" && !event.type_name().starts_with("model_")
            }),
            format!("expected no token_delta/model_* after turn_completed; got: {got}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("two-step Turn", &outcome.command)],
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
    let got = event_sequence(&outcome.events);
    println!("{}: {got}", spec.name);

    let result: Result<(), AssertError> =
        (|| {
            assert_clean_send(&outcome)?;
            assert_accepted_and_closed(&outcome)?;
            require(outcome.command.is_clean_exit(1), "failed Turn exits 1")?;
            require(
                outcome.events.iter().rev().any(|event| {
                    matches!(event, TurnEventLine::Unknown { type_name, raw }
                    if type_name == "snapshot"
                    && raw["view"]["selected_turn"]["summary"]["outcome"] == "failed"
                    && raw["view"]["selected_turn"]["summary"]["notice"]["level"] == "error")
                }),
                format!("expected durable Failed/Error summary; got: {got}"),
            )?;
            require(captures.len() == 3, "exactly three HTTP 500 requests")?;
            let retries: Vec<_> = outcome
                .events
                .iter()
                .enumerate()
                .filter_map(|(index, event)| {
                    if let TurnEventLine::LlmRetry { key, notice, .. } = event {
                        Some((index, key["attempt_ordinal"].as_u64().unwrap(), notice))
                    } else {
                        None
                    }
                })
                .collect();
            // 快照之前的事件不重发；重试通知只能是 [0, 1] 的后缀，
            // 最后一条重试通知与最终尝试的 model_started、turn_failed 都必须交付。
            let ordinals: Vec<_> = retries.iter().map(|(_, attempt, _)| *attempt).collect();
            require(
                !ordinals.is_empty() && [0, 1].ends_with(&ordinals),
                format!("expected a non-empty suffix of retry attempts [0, 1]; got: {got}"),
            )?;
            require(
                retries
                    .iter()
                    .all(|(_, _, notice)| notice["level"] == "warn"),
                format!("expected warn retry notices; got: {got}"),
            )?;
            let last_retry = retries.last().unwrap().0;
            let terminal = outcome.events.iter().position(|event| {
            matches!(event, TurnEventLine::Terminal { type_name, .. } if type_name == "turn_failed")
        });
            require(
                terminal.is_some_and(|index| {
                    index > last_retry + 1
                        && outcome.events[last_retry + 1..index]
                            .iter()
                            .filter(|event| matches!(event, TurnEventLine::ModelStarted { .. }))
                            .count()
                            == 1
                        && outcome.events[last_retry + 1..index]
                            .iter()
                            .all(|event| !matches!(event, TurnEventLine::LlmRetry { .. }))
                        && outcome.events.iter().enumerate().all(|(snapshot, event)| {
                            !is_summary_snapshot(event) || index < snapshot
                        })
                }),
                format!(
                    "expected final model_started then turn_failed after last llm_retry; got: {got}"
                ),
            )?;
            let TurnEventLine::Terminal { raw, .. } = &outcome.events[terminal.unwrap()] else {
                unreachable!()
            };
            let attempts = raw["step"]["attempts"].as_array().unwrap();
            require(
                attempts.len() == 3
                    && raw["summary"]["notice"]["level"] == "error"
                    && raw["summary"]["notice"]["text"] == attempts[2]["end"]["diagnostic"],
                "terminal notice must equal the final attempt diagnostic",
            )
        })();
    finish_scenario(
        evidence,
        &[("exhausted retries", &outcome.command)],
        captures,
        result,
    );
}
