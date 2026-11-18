// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn structured_result_wire() {
    let wire = json!({
        "invocation_id": "00000000-0000-4000-8000-000000000001",
        "name": "observe__workspace",
        "payload": {"outcome":"succeeded", "payload": {
            "kind":"output_too_large", "digest":"ab".repeat(32), "byte_length":65537
        }}
    });
    let neutral: lattice_conversation::StoredToolResult =
        serde_json::from_value(wire.clone()).unwrap();
    let stored: StoredToolResult = neutral;
    assert_eq!(serde_json::to_value(&stored).unwrap(), wire);
    assert_eq!(stored.payload.outcome(), ToolResultOutcome::Succeeded);
    let digest = lattice_conversation::tool_content_digest("{}")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert_eq!(
        digest,
        "75f9393780337294f4a878b86195ce835e1af90ba8f3a63c2702f6d6e42f29f4"
    );
}

#[test]
fn domain_types_v10_wire() {
    let settings_wire = json!({
        "profile": {
            "agent_name": "coder",
            "description": "coding agent",
            "default_model": "openai/org/gpt-5",
            "default_effort": "medium",
            "timeout_secs": 300,
            "retry": {
                "max_retries": 3,
                "initial_delay_ms": 500,
                "max_delay_ms": 3000
            }
        },
        "system_prompt": "系统提示",
        "model": "openai/org/gpt-5",
        "effort": "medium"
    });
    let settings: SessionSettings = serde_json::from_value(settings_wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&settings).unwrap(), settings_wire);
    let encoded_settings = encode(&settings).unwrap();
    assert_eq!(&encoded_settings[..4], &10_u32.to_be_bytes());
    assert_eq!(
        decode_session_settings("s1", &encoded_settings).unwrap(),
        settings
    );

    let user_wire = json!({
        "kind": "user_message",
        "parent": 0,
        "turn_id": "00000000-0000-4000-8000-000000000001",
        "accepted_at": "2026-09-27T00:00:00Z",
        "agent_name": "coder",
        "model": "org/gpt-5",
        "provider": "openai",
        "effort": "medium",
        "limits": {
            "timeout_secs": 300,
            "max_model_steps": 32,
            "max_model_attempts": 4
        },
        "config_digest": "cd".repeat(32),
        "message": {
            "role": "user",
            "provider_origin": null,
            "parts": [
                {
                    "semantic": { "text": { "text": "hello" } },
                    "replay_state": null
                }
            ]
        }
    });
    let user: Node = serde_json::from_value(user_wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&user).unwrap(), user_wire);
    let encoded_user = encode(&user).unwrap();
    assert_eq!(&encoded_user[..4], &10_u32.to_be_bytes());
    assert!(decode::<Node>("nodes", "n1", &encoded_user).unwrap() == user);
}

#[test]
fn keys_versions_and_encoding() {
    for n in [0, 1, 255, 256, u64::MAX] {
        assert_eq!(
            decode_node_key(&node_key(sid(1), NodeId(n))).unwrap(),
            (sid(1), NodeId(n))
        );
    }
    assert!(node_key(sid(1), NodeId(255)) < node_key(sid(1), NodeId(256)));
    assert!(node_key(sid(1), NodeId(u64::MAX)) < node_key(sid(2), NodeId(0)));
    for bad in [
        vec![],
        vec![b'a'; 39],
        vec![b'a'; 41],
        vec![b'A'; 40],
        vec![255; 40],
    ] {
        assert!(decode_node_key(&bad).is_err());
    }
    for digest in [[0; 32], [255; 32], [171; 32]] {
        assert_eq!(decode_config_key(&config_key(&digest)).unwrap(), digest);
    }
    for key in [
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        assert!(decode_config_key(&key).is_err());
    }
    assert_eq!(
        decode::<Node>("nodes", "0", &encode(&Node::Root).unwrap()).unwrap(),
        Node::Root
    );
    for version in (0..VERSION).chain([VERSION + 1, u32::MAX]) {
        let mut bytes = version.to_be_bytes().to_vec();
        bytes.extend(b"{\"kind\":\"root\"}");
        let error = attach_path(
            std::path::Path::new("/test/lattice.redb"),
            decode::<Node>("nodes", "0", &bytes),
        )
        .unwrap_err();
        assert!(
            matches!(error, StoreError::UnsupportedVersion { path, found, supported: VERSION, .. } if path == std::path::Path::new("/test/lattice.redb") && found == version)
        );
    }
    for body in [
        b"".as_slice(),
        b"\xff",
        b"null",
        b"{}",
        b"{\"kind\":\"private-variant\"}",
        b"{\"kind\":\"root\",\"private-field\":true}",
        b"{\"kind\":\"root\"} trailing",
    ] {
        let mut bytes = VERSION.to_be_bytes().to_vec();
        bytes.extend(body);
        let error = decode::<Node>("nodes", "0", &bytes).unwrap_err();
        assert!(!format!("{error:?}").contains("private-"));
    }
    for bytes in [vec![], vec![0], vec![0; 3]] {
        assert!(decode::<Node>("nodes", "0", &bytes).is_err());
    }
    let (_d, _l, db) = store();
    {
        let read = db.begin_read().unwrap();
        let meta = read.tx.open_table(META).unwrap();
        assert_eq!(
            meta.get("store_format_version").unwrap().unwrap().value(),
            b"\x00\x00\x00\x0a10"
        );
    }
    let (_lock, _calls) = running(&db, sid(1), tid(1), 1, 1);
    let r = records(&db);
    assert_eq!(
        decode::<SessionRecord>("sessions", "1", &encode(&r.sessions[&sid(1)]).unwrap()).unwrap(),
        r.sessions[&sid(1)]
    );
    assert_eq!(
        decode::<Node>(
            "nodes",
            "2",
            &encode(&r.nodes[&(sid(1), NodeId(2))]).unwrap()
        )
        .unwrap(),
        r.nodes[&(sid(1), NodeId(2))]
    );
    assert_eq!(
        decode::<StoredTurnConfig>("configs", "1", &encode(&config().stored()).unwrap()).unwrap(),
        config().stored()
    );
    assert_eq!(
        decode::<NodeRef>("turn_index", "1", &encode(&r.turn_index[&tid(1)]).unwrap()).unwrap(),
        r.turn_index[&tid(1)]
    );
    assert_eq!(
        decode::<InvocationIndexEntry>(
            "invocation_index",
            "1",
            &encode(&r.index[&iid(1)]).unwrap()
        )
        .unwrap(),
        r.index[&iid(1)]
    );
    for (usage_kind, cost) in [
        (UsageCompleteness::Partial, None),
        (
            UsageCompleteness::Complete,
            Some(CostEstimate {
                usd: 0.0,
                exact: true,
            }),
        ),
        (
            UsageCompleteness::Partial,
            Some(CostEstimate {
                usd: 0.125,
                exact: false,
            }),
        ),
    ] {
        let mut node = r.nodes[&(sid(1), NodeId(2))].clone();
        let Node::Step { attempts, .. } = &mut node else {
            unreachable!()
        };
        attempts[0].usage = Some(TokenUsage {
            input: Some(10),
            output: Some(2),
            ..Default::default()
        });
        attempts[0].usage_kind = usage_kind;
        attempts[0].cost = cost;
        let bytes = encode(&node).unwrap();
        assert_eq!(decode::<Node>("nodes", "2", &bytes).unwrap(), node);
    }
    for invalid_cost in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut node = r.nodes[&(sid(1), NodeId(2))].clone();
        let Node::Step { attempts, .. } = &mut node else {
            unreachable!()
        };
        attempts[0].cost = Some(CostEstimate {
            usd: invalid_cost,
            exact: true,
        });
        assert!(encode(&node).is_err());
    }
    let mut damaged = serde_json::to_value(&r.nodes[&(sid(1), NodeId(2))]).unwrap();
    damaged["attempts"][0]["cost"] = json!({"usd":-1.0,"exact":true});
    let mut bytes = VERSION.to_be_bytes().to_vec();
    bytes.extend(serde_json::to_vec(&damaged).unwrap());
    assert!(matches!(
        decode::<Node>("nodes", "2", &bytes),
        Err(StoreError::Corruption { .. })
    ));
    for invalid in ["-1", "1.5", r#""many""#, "18446744073709551616"] {
        let mut damaged = serde_json::to_value(&r.nodes[&(sid(1), NodeId(2))]).unwrap();
        damaged["attempts"][0]["usage"]["input"] = json!(0);
        // 直接写入原始数字，保留超范围整数的十进制编码。
        let raw = serde_json::to_string(&damaged)
            .unwrap()
            .replace("\"input\":0", &format!("\"input\":{invalid}"));
        let mut bytes = VERSION.to_be_bytes().to_vec();
        bytes.extend(raw.as_bytes());
        assert!(
            matches!(
                decode::<Node>("nodes", "2", &bytes),
                Err(StoreError::Corruption { .. })
            ),
            "计数字段损坏未被拒绝：{invalid}"
        );
    }
}

#[test]
fn nested_unknown_fields_are_rejected() {
    let (_d, _l, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    finish(
        &db,
        &lock,
        &calls.bindings[0],
        failed_reply("execution_failed"),
    );
    let original = serde_json::to_value(node_at(&db, 2)).unwrap();
    for pointer in [
        "/attempts/0/origin",
        "/attempts/0/usage",
        "/attempts/0/progress",
        "/attempts/0/progress/end",
        "/reply",
        "/reply/parts/1",
        "/reply/parts/1/semantic/tool_call/call",
        "/slots/0",
        "/slots/0/binding",
        "/slots/0/binding/identity",
        "/slots/0/progress",
        "/slots/0/progress/result/payload/payload",
    ] {
        let mut v = original.clone();
        v.pointer_mut(pointer)
            .unwrap_or_else(|| panic!("fixture {pointer}"))
            .as_object_mut()
            .unwrap()
            .insert("private-field".into(), json!(true));
        assert!(
            matches!(
                decode::<Node>("nodes", "2", &encode(&v).unwrap()),
                Err(StoreError::Corruption { .. })
            ),
            "{pointer}"
        );
    }
    for pointer in [
        "/attempts/0/origin",
        "/attempts/0/origin/protocol",
        "/attempts/0/origin/provider_id",
    ] {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = serde_json::Value::Null;
        assert!(
            decode::<Node>("nodes", "2", &encode(&value).unwrap()).is_err(),
            "{pointer}"
        );
    }
    let mut value = original;
    value["attempts"][0]["origin"]["protocol"] = json!("unknown");
    assert!(decode::<Node>("nodes", "2", &encode(&value).unwrap()).is_err());
    for pointer in ["", "/catalog", "/catalog/entries/0"] {
        let mut v = serde_json::to_value(config().stored()).unwrap();
        v.pointer_mut(pointer).unwrap()["private-field"] = json!(true);
        assert!(decode::<StoredTurnConfig>("configs", "key", &encode(&v).unwrap()).is_err());
    }
}

#[test]
fn json_payload_field_names_are_not_store_records() {
    let (_dir, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    let value = json!({"evidence":{"kind":"recorded","value":{"custom":true},"custom":true}});
    db.start_invocation(&lock, &calls.bindings[0], now())
        .unwrap();
    let terminal = prepare_terminal(&calls.bindings[0], ToolReply::Succeeded(value.clone()));
    db.finish_invocation(
        &lock,
        TerminalInput {
            invocation_id: iid(1),
            terminal,
        },
        now(),
    )
    .unwrap();
    db.close_step(&lock, CloseStep::Continue { key: calls.key }, now())
        .unwrap();
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    let content = node_at(&db, 2).step_content().unwrap();
    assert!(
        matches!(&content.results[0].payload, ResultPayload::Succeeded(SuccessContent::Json { value: actual, .. }) if *actual == value)
    );
}
