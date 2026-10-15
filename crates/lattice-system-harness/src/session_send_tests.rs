use super::*;
use serde_json::json;

fn terminal(outcome: &str, notice: Value) -> Value {
    let end = if outcome == "completed" {
        json!({"end":"finish"})
    } else {
        json!({"end":"stop","cause":{"cause":if outcome=="cancelled" {"cancelled"} else {"process_exit"}}})
    };
    let progress = json!({"progress":"closed","end":end});
    json!({"type":format!("turn_{outcome}"),"turn":{"session_id":"s","turn_id":"t"},
        "step":{"node":2,"key":{"turn_id":"t","ordinal":0},"attempts":[],"progress":progress,
            "end":end,"slots":[],"updated_at":"2026-09-27T00:00:01Z"},
        "summary":{"outcome":outcome,"notice":notice,"progress":progress}})
}
fn history() -> Value {
    let summary = terminal("completed", Value::Null)["summary"].clone();
    json!({"type":"history","history":{"anchor":{"session_id":"s","head":2},"entries":[
        {"node":1,"agent_name":"reader","summary":summary,"retries":[],"content":{"kind":"user_message","parent":0,"turn_id":"t","message":{"parts":[]}}},
        {"node":2,"agent_name":"reader","summary":summary,"retries":[],"content":{"kind":"step","parent":1,"turn_id":"t","step_ordinal":0,"reply":{"parts":[]},"slots":[],"progress":summary["progress"]}}
    ]}})
}
fn parse(values: &[Value]) -> SendOutcome {
    parse_send_stdout(
        values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
        Some(0),
    )
}

#[test]
fn r2_send_exit_code_accounts_for_outcome_and_delivery() {
    let done = terminal("completed", Value::Null);
    let snapshot = json!({"type":"snapshot","view":{"owned":false,"session":{"session_id":"s","current":2},"history":{"session_id":"s","head":2},"selected_turn":{"session_id":"s","turn_id":"t","summary":done["summary"],"steps":[done["step"].clone()]}}});
    let mut mismatched_snapshot = snapshot.clone();
    mismatched_snapshot["view"]["history"]["head"] = json!(3);
    mismatched_snapshot["view"]["session"]["current"] = json!(3);
    for (name, values, expected_exit_code, expected_malformed_count) in [
        (
            "completed_with_history",
            vec![done.clone(), history()],
            0,
            Some(0),
        ),
        (
            "failed_with_history",
            vec![
                terminal("failed", json!({"level":"error","text":"进程退出"})),
                history(),
            ],
            1,
            Some(0),
        ),
        (
            "cancelled_with_history",
            vec![
                terminal("cancelled", json!({"level":"info","text":"用户取消"})),
                history(),
            ],
            2,
            Some(0),
        ),
        ("completed_without_history", vec![done.clone()], 1, None),
        (
            "completed_then_history_read_error",
            vec![
                done.clone(),
                history(),
                json!({"type":"error","kind":"history","message":"unavailable"}),
            ],
            1,
            None,
        ),
        (
            "history_before_matching_snapshot",
            vec![history(), snapshot],
            0,
            Some(0),
        ),
        (
            "history_before_mismatched_snapshot",
            vec![history(), mismatched_snapshot],
            1,
            None,
        ),
        (
            "store_unavailable_before_accepted",
            vec![
                json!({"type":"error","kind":"store_unavailable","message":"session store unavailable"}),
                json!({"type":"acceptance","result":{"acceptance":"accepted","turn":{"session_id":"s","turn_id":"t"}}}),
                done,
                history(),
            ],
            0,
            Some(0),
        ),
    ] {
        let result = parse(&values);
        if let Some(count) = expected_malformed_count {
            assert_eq!(
                result.malformed_lines.len(),
                count,
                "{name}: {:?}",
                result.malformed_lines
            );
        }
        assert_eq!(
            derive_send_exit_code(&result.events).as_i32(),
            expected_exit_code,
            "{name}"
        );
    }
}

#[test]
fn r2_rejects_bad_summary_in_event_and_snapshot() {
    for mut summary in [
        json!({"outcome":"completed"}),
        json!({"outcome":"completed","notice":{"level":"info","text":"wrong"}}),
        json!({"outcome":"failed","notice":null}),
        json!({"outcome":"failed","notice":{"level":"info","text":"wrong"}}),
        json!({"outcome":"cancelled","notice":{"level":"error","text":"wrong"}}),
        json!({"outcome":{"completed":{},"failed":{}},"notice":null}),
        json!({"outcome":"future","notice":null}),
    ] {
        let mut event = terminal("completed", Value::Null);
        summary["progress"] = event["step"]["progress"].clone();
        event["summary"] = summary.clone();
        let snapshot = json!({"type":"snapshot","view":{"owned":false,"session":{"session_id":"s","current":2},"history":{"session_id":"s","head":2},"selected_turn":{"turn_id":"t","session_id":"s","summary":summary,"steps":[event["step"].clone()]}}});
        for value in [event, snapshot] {
            let result = parse(&[value]);
            assert_eq!(
                result.malformed_lines.len(),
                1,
                "accepted {:?}",
                result.events
            );
            assert_eq!(derive_send_exit_code(&result.events).as_i32(), 1);
        }
    }
}

#[test]
fn r2_unknown_and_malformed_records_remain_observable() {
    let parsed = parse_send_stdout(
        b"{\"type\":\"future\",\"x\":4}\n{\"type\":\"token_delta\",\"content\":7}\n\xff\n",
        Some(0),
    );
    assert!(matches!(&parsed.events[0],TurnEventLine::Unknown{raw,..} if raw["x"]==4));
    assert_eq!(parsed.malformed_lines.len(), 2);
    assert_eq!(parsed.malformed_lines[1].raw, vec![255]);
}

#[test]
fn retry_notice_requires_warn_and_an_unsigned_delay() {
    let value = json!({
        "type": "llm_retry",
        "key": {"turn_id": "t", "model_step_ordinal": 0, "attempt_ordinal": 1},
        "delay_ms": 10,
        "notice": {"level": "warn", "text": "provider diagnostic"}
    });
    let parsed = parse(std::slice::from_ref(&value));
    assert!(parsed.malformed_lines.is_empty());
    assert!(
        matches!(&parsed.events[0], TurnEventLine::LlmRetry { attempt_ordinal, delay_ms, level, text }
        if *attempt_ordinal == 1 && *delay_ms == 10 && level == "warn" && text == "provider diagnostic")
    );
    for (field, invalid) in [
        ("delay_ms", Value::Null),
        ("delay_ms", json!(-1)),
        ("notice", json!({"level":"error","text":"wrong"})),
        ("notice", json!({"level":"warn","text":7})),
    ] {
        let mut bad = value.clone();
        bad[field] = invalid;
        assert_eq!(parse(&[bad]).malformed_lines.len(), 1);
    }
}
