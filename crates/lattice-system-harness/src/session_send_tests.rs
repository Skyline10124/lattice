use super::*;
use serde_json::json;

fn terminal(outcome: &str, notice: Value) -> Value {
    json!({"type":format!("turn_{outcome}"),"turn":{"session_id":"s","turn_id":"t"},
        "step":{"key":{"turn_id":"t","ordinal":0},"attempts":[],
            "state":{"state":"closed","end":if outcome=="completed" {json!({"end":"finish","node":2})} else {json!({"end":"stop","node":null,"cause":if outcome=="cancelled" {"cancelled"}else{"process_exit"}})},"slots":[]}},
        "summary":{"outcome":outcome,"notice":notice}})
}
fn history() -> Value {
    json!({"type":"history","history":{"anchor":{"session_id":"s","head":2},"entries":[
        {"node":1,"agent_name":"reader","content":{"type":"user_message","parent":0,"turn_id":"t","message":{"parts":[]}}},
        {"node":2,"agent_name":"reader","content":{"type":"step","parent":1,"turn_id":"t","step_ordinal":0,"content":{"assistant":{"parts":[]},"results":[]}}}
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
fn r2_summary_and_delivery_classify_three_outcomes() {
    for (outcome, notice, code) in [
        ("completed", Value::Null, 0),
        ("failed", json!({"level":"error","text":"进程退出"}), 1),
        ("cancelled", json!({"level":"info","text":"用户取消"}), 2),
    ] {
        let result = parse(&[terminal(outcome, notice), history()]);
        assert!(
            result.malformed_lines.is_empty(),
            "{:?}",
            result.malformed_lines
        );
        assert_eq!(
            derive_send_exit_code(&result.events).as_i32(),
            code,
            "{outcome}"
        );
    }
}

#[test]
fn r2_rejects_bad_summary_in_event_and_snapshot() {
    for summary in [
        json!({"outcome":"completed"}),
        json!({"outcome":"completed","notice":{"level":"info","text":"wrong"}}),
        json!({"outcome":"failed","notice":null}),
        json!({"outcome":"failed","notice":{"level":"info","text":"wrong"}}),
        json!({"outcome":"cancelled","notice":{"level":"error","text":"wrong"}}),
        json!({"outcome":{"completed":{},"failed":{}},"notice":null}),
        json!({"outcome":"future","notice":null}),
    ] {
        let mut event = terminal("completed", Value::Null);
        event["summary"] = summary.clone();
        let snapshot = json!({"type":"snapshot","view":{"session":{"session_id":"s","current":2,"active_turn":null},"history":{"session_id":"s","head":2},"selected_turn":{"turn_id":"t","session_id":"s","summary":summary,"steps":[event["step"].clone()]}}});
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
fn r2_completed_does_not_hide_missing_history_or_read_error() {
    let done = terminal("completed", Value::Null);
    assert_eq!(
        derive_send_exit_code(&parse(std::slice::from_ref(&done)).events).as_i32(),
        1
    );
    let result = parse(&[
        done,
        history(),
        json!({"type":"error","kind":"history","message":"unavailable"}),
    ]);
    assert_eq!(derive_send_exit_code(&result.events).as_i32(), 1);
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
fn r2_history_delivery_tracks_anchor_not_snapshot_arrival() {
    let done = terminal("completed", Value::Null);
    let mut snapshot = json!({"type":"snapshot","view":{"session":{"session_id":"s","current":2},"history":{"session_id":"s","head":2},"selected_turn":{"session_id":"s","turn_id":"t","summary":done["summary"],"steps":[done["step"].clone()]}}});
    let result = parse(&[history(), snapshot.clone()]);
    assert!(result.malformed_lines.is_empty());
    assert_eq!(derive_send_exit_code(&result.events).as_i32(), 0);
    snapshot["view"]["history"]["head"] = json!(3);
    snapshot["view"]["session"]["current"] = json!(3);
    assert_eq!(
        derive_send_exit_code(&parse(&[history(), snapshot]).events).as_i32(),
        1
    );
}

#[test]
fn r2_observation_failure_before_acceptance_is_not_execution_failure() {
    let result = parse(&[
        json!({"type":"error","kind":"store_unavailable","message":"session store unavailable"}),
        json!({"type":"acceptance","result":{"acceptance":"accepted","turn":{"session_id":"s","turn_id":"t"}}}),
        terminal("completed", Value::Null),
        history(),
    ]);
    assert!(result.malformed_lines.is_empty());
    assert_eq!(derive_send_exit_code(&result.events).as_i32(), 0);
}
