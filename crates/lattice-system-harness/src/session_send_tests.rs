use super::*;
use serde_json::json;

fn closed(end: Value) -> Value {
    json!({"progress":"closed","end":end})
}
fn finish() -> Value {
    closed(json!({"end":"finish"}))
}
fn stop(cause: &str) -> Value {
    closed(json!({"end":"stop","cause":{"cause":cause}}))
}
fn summary(outcome: &str, progress: &Value) -> Value {
    let notice = match outcome {
        "completed" => Value::Null,
        "failed" => json!({"level":"error","text":"进程退出"}),
        _ => json!({"level":"info","text":"用户取消"}),
    };
    json!({"outcome":outcome,"notice":notice,"progress":progress})
}
fn user(node: u64, ordinal: u64) -> Value {
    json!({"type":"entry","node":node,"ordinal":ordinal,"content":{"kind":"user_message","parent":node - 1,
        "turn_id":"t","accepted_at":"2026-10-16T00:00:00Z","agent_name":"reader","model":"m","provider":"p",
        "limits":{},"config_digest":"00","message":{"role":"user","parts":[{"semantic":{"text":{"text":"hi"}},"replay_state":null}]}}})
}
/// Step 条目；`summary` 为 None 时不带该字段。
fn step(node: u64, ordinal: u64, progress: Value, summary: Option<Value>, reply: &str) -> Value {
    let end = match &progress["end"] {
        Value::Null => json!({"end":"stop","cause":{"cause":"process_exit"}}),
        end => end.clone(),
    };
    let mut entry = json!({"type":"entry","node":node,"retries":[],
        "content":{"kind":"step","parent":node - 1,"turn_id":"t","step_ordinal":ordinal,"progress":progress,
            "attempts":[],"slots":[],"updated_at":"2026-10-16T00:00:01Z",
            "reply":{"role":"assistant","parts":[{"semantic":{"text":{"text":reply}},"replay_state":null}]}},
        "step":{"node":node,"key":{"turn_id":"t","ordinal":ordinal},"progress":progress,"end":end,
            "attempts":[],"slots":[],"updated_at":"2026-10-16T00:00:01Z"}});
    if let Some(summary) = summary {
        entry["summary"] = summary;
    }
    entry
}
fn head(owner: bool) -> Value {
    json!({"type":"head","session_id":"s","position":3,"agent":"reader","turn_id":"t","owner":owner,
        "diagnostic":null,"path_replaced":false})
}
fn sending() -> Value {
    json!({"type":"submit","stage":"sending","turn_id":"t","session_id":"s","new":true})
}
fn accepted() -> Value {
    json!({"type":"submit","stage":"receipt","turn_id":"t","result":"accepted","recovered":false,
        "turn":{"session_id":"s","turn_id":"t","user_node":1}})
}
fn rejected(kind: &str, conflict: Option<&str>) -> Value {
    json!({"type":"submit","stage":"receipt","turn_id":"t","result":"rejected","recovered":false,
        "error":{"kind":kind,"message":"rejected","conflict":conflict}})
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
fn exit_code_follows_the_latest_value_of_the_last_step() {
    let requesting = json!({"progress":"requesting"});
    let accepted_turn = vec![sending(), accepted(), head(true), user(1, 1)];
    let with = |tail: Vec<Value>| [accepted_turn.clone(), tail].concat();
    let done = step(
        2,
        0,
        finish(),
        Some(summary("completed", &finish())),
        "answer",
    );
    let continued = step(2, 0, closed(json!({"end":"continue"})), None, "first");
    let next = step(
        3,
        1,
        requesting.clone(),
        Some(summary("failed", &requesting)),
        "",
    );
    for (name, values, expected, result) in [
        (
            "completed",
            with(vec![
                step(
                    2,
                    0,
                    requesting.clone(),
                    Some(summary("failed", &requesting)),
                    "",
                ),
                done.clone(),
            ]),
            0,
            Some("completed"),
        ),
        // 结局按 node 覆盖后的最新值计算，同一条目重复推送不改变结果。
        (
            "repeated_final_entry",
            with(vec![done.clone(), done.clone()]),
            0,
            Some("completed"),
        ),
        (
            "failed",
            with(vec![step(
                2,
                0,
                stop("provider_error"),
                Some(summary("failed", &stop("provider_error"))),
                "",
            )]),
            1,
            Some("failed"),
        ),
        (
            "cancelled",
            with(vec![step(
                2,
                0,
                stop("cancelled"),
                Some(summary("cancelled", &stop("cancelled"))),
                "",
            )]),
            2,
            Some("cancelled"),
        ),
        (
            "still_running",
            with(vec![continued.clone(), next.clone()]),
            1,
            None,
        ),
        (
            "owner_vanished",
            with(vec![continued.clone(), next.clone(), head(false)]),
            1,
            Some("failed"),
        ),
        (
            "completed_then_read_error",
            with(vec![
                done.clone(),
                json!({"type":"error","kind":"internal","message":"read failed"}),
            ]),
            1,
            Some("completed"),
        ),
        (
            "turn_id_conflict",
            vec![sending(), rejected("conflict", Some("turn_id_used"))],
            2,
            None,
        ),
        ("busy", vec![sending(), rejected("busy", None)], 1, None),
        (
            "not_accepted",
            vec![
                sending(),
                json!({"type":"submit","stage":"receipt","turn_id":"t","result":"not_accepted","recovered":true}),
            ],
            1,
            None,
        ),
        (
            "unknown",
            vec![
                sending(),
                json!({"type":"submit","stage":"receipt","turn_id":"t","result":"unknown","recovered":false}),
            ],
            1,
            None,
        ),
    ] {
        let parsed = parse(&values);
        let malformed = &parsed.malformed_lines;
        assert!(malformed.is_empty(), "{name}: {malformed:?}");
        assert_eq!(
            parsed.closed_result().map(|(_, _, outcome)| outcome),
            result,
            "{name}"
        );
        assert_eq!(
            derive_send_exit_code(&parsed.records).as_i32(),
            expected,
            "{name}"
        );
    }
    let last = step(
        3,
        1,
        finish(),
        Some(summary("completed", &finish())),
        "answer",
    );
    let parsed = parse(&with(vec![continued, last]));
    assert_eq!(parsed.closed_result(), Some(("s", "t", "completed")));
    assert_eq!(parsed.sending(), Some(("t", "s", true)));
    assert_eq!(parsed.accepted_session_id(), Some("s"));
    assert_eq!(parsed.reply_text(), "firstanswer");
}

#[test]
fn summaries_are_checked_and_only_the_last_step_carries_one() {
    let mut bad = Vec::new();
    for notice in [
        json!({"level":"info","text":"wrong"}),
        Value::Null,
        json!({"level":"error","text":""}),
    ] {
        let mut wrong = summary("failed", &stop("provider_error"));
        wrong["notice"] = notice;
        bad.push(step(2, 0, stop("provider_error"), Some(wrong), ""));
    }
    for (progress, outcome) in [(finish(), "failed"), (stop("cancelled"), "failed")] {
        bad.push(step(
            2,
            0,
            progress.clone(),
            Some(summary(outcome, &progress)),
            "",
        ));
    }
    let mut stale = step(2, 0, finish(), Some(summary("completed", &finish())), "");
    stale["summary"]["progress"] = json!({"progress":"requesting"});
    bad.push(stale);
    let continued = closed(json!({"end":"continue"}));
    bad.push(step(
        2,
        0,
        continued.clone(),
        Some(summary("completed", &finish())),
        "",
    ));
    bad.push(step(2, 0, finish(), None, ""));
    let mut detached = step(2, 0, finish(), Some(summary("completed", &finish())), "");
    detached["step"]["node"] = json!(9);
    bad.push(detached);
    let mut numbered = step(2, 0, continued, None, "");
    numbered["ordinal"] = json!(1);
    bad.push(numbered);
    let mut zero = user(1, 1);
    zero["ordinal"] = json!(0);
    bad.push(zero);
    for value in bad {
        let parsed = parse(&[sending(), accepted(), head(true), user(1, 1), value.clone()]);
        assert_eq!(parsed.malformed_lines.len(), 1, "accepted {value}");
        assert_ne!(derive_send_exit_code(&parsed.records).as_i32(), 0);
    }
}

#[test]
fn unknown_and_malformed_records_remain_observable() {
    let parsed = parse_send_stdout(
        b"{\"type\":\"future\",\"x\":4}\n{\"type\":\"text\",\"node\":\"x\"}\n{\"kind\":\"error\"}\n\xff\n",
        Some(0),
    );
    assert!(
        matches!(&parsed.records[..], [SendRecord::Unknown { type_name, raw }] if type_name == "future" && raw["x"] == 4)
    );
    assert_eq!(parsed.malformed_lines.len(), 3);
    assert_eq!(parsed.malformed_lines[2].raw, vec![255]);
}

#[test]
fn retry_notice_requires_warn_and_an_unsigned_delay() {
    let mut entry = step(2, 0, finish(), Some(summary("completed", &finish())), "");
    let notice =
        json!({"attempt":0,"delay_ms":10,"notice":{"level":"warn","text":"provider diagnostic"}});
    entry["retries"] = json!([notice]);
    let parsed = parse(std::slice::from_ref(&entry));
    assert!(
        parsed.malformed_lines.is_empty(),
        "{:?}",
        parsed.malformed_lines
    );
    assert_eq!(
        parsed.entries()[0].step().unwrap().retries,
        vec![RetryNotice {
            attempt: 0,
            delay_ms: 10,
            text: "provider diagnostic".into()
        }]
    );
    for (field, invalid) in [
        ("delay_ms", Value::Null),
        ("delay_ms", json!(-1)),
        ("notice", json!({"level":"error","text":"wrong"})),
        ("notice", json!({"level":"warn","text":7})),
    ] {
        let mut bad = entry.clone();
        bad["retries"][0][field] = invalid;
        assert_eq!(parse(&[bad]).malformed_lines.len(), 1);
    }
}

#[test]
fn conflict_reason_accompanies_exactly_the_conflict_kind() {
    for (kind, conflict, valid) in [
        ("conflict", Some("session_exists"), true),
        ("conflict", None, false),
        ("conflict", Some("other"), false),
        ("busy", Some("turn_id_used"), false),
        ("busy", None, true),
    ] {
        let parsed = parse(&[rejected(kind, conflict)]);
        assert_eq!(
            parsed.malformed_lines.is_empty(),
            valid,
            "{kind} {conflict:?}"
        );
    }
    // 拒绝只在 `sending` 之后紧跟一条 `rejected` 回执、没有别的记录时成立。
    let busy = parse(&[sending(), rejected("busy", None)]);
    assert_eq!(
        busy.rejection().map(|error| error.kind.as_str()),
        Some("busy")
    );
    let extra = json!({"type":"error","kind":"busy","message":"rejected"});
    assert!(
        parse(&[sending(), rejected("busy", None), extra.clone()])
            .rejection()
            .is_none()
    );
    assert!(parse(&[sending(), extra]).rejection().is_none());
}

#[test]
fn view_state_appends_text_and_rejects_regression() {
    let text = |offset: u64, segments: Value| {
        parse_record(
            &json!({"type":"text","node":2,"attempt":0,"offset":offset,"segments":segments}),
        )
        .unwrap()
    };
    let mut view = ViewState::default();
    view.apply(&text(0, json!([{"kind":"text","text":"中a"}])))
        .unwrap();
    view.apply(&text(
        4,
        json!([{"kind":"text","text":"b"},{"kind":"reasoning","text":"r"}]),
    ))
    .unwrap();
    assert_eq!(
        view.texts[&(2, 0)],
        vec![
            Segment {
                kind: SegmentKind::Text,
                text: "中ab".into()
            },
            Segment {
                kind: SegmentKind::Reasoning,
                text: "r".into()
            },
        ]
    );
    assert!(
        view.apply(&text(5, json!([{"kind":"text","text":"x"}])))
            .is_err()
    );
    // 同一连接内已收到文字后，偏移不会再从 0 开始。
    assert!(
        view.apply(&text(0, json!([{"kind":"text","text":"x"}])))
            .is_err()
    );
    let done = parse_record(&step(
        2,
        0,
        finish(),
        Some(summary("completed", &finish())),
        "",
    ))
    .unwrap();
    let requesting = json!({"progress":"requesting"});
    let earlier = parse_record(&step(
        2,
        0,
        requesting.clone(),
        Some(summary("failed", &requesting)),
        "",
    ))
    .unwrap();
    view.apply(&done).unwrap();
    assert!(view.apply(&earlier).is_err());
    let mut replaced = head(false);
    replaced["path_replaced"] = json!(true);
    view.apply(&parse_record(&replaced).unwrap()).unwrap();
    assert!(view.entries.is_empty() && view.texts.is_empty());
}

#[test]
fn view_state_keeps_written_facts_and_allows_repeated_values() {
    let executing = json!({"progress":"executing"});
    let attempt = json!({"attempt":0,"origin":"primary","usage":null,
        "progress":{"progress":"ended","end":{"end":"completed"},"diagnostic":null},
        "end":{"end":"completed","step":{"turn_id":"t","model_step_ordinal":0}},"retry_delay_ms":null});
    let slot = |id: &str, state: &str, outcome: &str| {
        json!({"invocation_id":id,"binding":{"method":"observe.workspace"},
            "progress":{"state":state},"outcome":outcome,"reason":null})
    };
    let mut base = step(
        2,
        0,
        executing.clone(),
        Some(summary("failed", &executing)),
        "reply",
    );
    base["content"]["attempts"] = json!([{}]);
    base["step"]["attempts"] = json!([attempt]);
    base["step"]["slots"] = json!([
        slot("a", "terminal", "succeeded"),
        slot("b", "registered", "not_executed")
    ]);
    let parsed = |value: &Value| parse_record(value).unwrap();
    // 记录去掉 `type` 后就是历史查询给出的同一个 Entry。
    let SendRecord::Entry(entry) = parsed(&base) else {
        panic!("entry record");
    };
    let mut stored = base.clone();
    stored.as_object_mut().unwrap().remove("type");
    assert_eq!(entry.raw, stored);
    assert_eq!(parse_entry(&stored).unwrap(), *entry);
    let mut view = ViewState::default();
    view.apply(&parsed(&base)).unwrap();
    // 同一个值重推，以及尚未终态的槽位前进，都是延续。
    view.apply(&parsed(&base)).unwrap();
    let mut started = base.clone();
    started["step"]["slots"][1] = slot("b", "started", "indeterminate");
    view.apply(&parsed(&started)).unwrap();
    for (pointer, changed) in [
        ("/step/attempts/0/retry_delay_ms", json!(5)),
        ("/step/slots/0/outcome", json!("failed")),
        ("/step/slots/1/progress/state", json!("registered")),
        (
            "/content/reply/parts/0/semantic/text/text",
            json!("rewritten"),
        ),
    ] {
        let mut value = started.clone();
        *value.pointer_mut(pointer).unwrap() = changed;
        assert!(view.clone().apply(&parsed(&value)).is_err(), "{pointer}");
    }
}
