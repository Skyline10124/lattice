// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn observer_projections_skip_unrelated_content() {
    for (node, pointer) in [
        (NodeId(2), "/reply"),
        (NodeId(2), "/slots"),
        (NodeId(2), "/attempts/0/usage"),
        (NodeId(2), "/attempts/0/progress/diagnostic"),
        (NodeId(2), "/turn_start/config_digest"),
        (NodeId(2), "/turn_start/limits"),
        (NodeId(1), "/input"),
    ] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
            .unwrap();
        db.stop_attempt(
            &lock,
            stopped(
                key(tid(1), 0, 0),
                StopCause::ProviderError { retryable: true },
            ),
            now(),
        )
        .unwrap();
        db.start_request(&lock, key(tid(1), 0, 1), origin(), now())
            .unwrap();
        edit_projection_node(&db, node, |wire| {
            *wire.pointer_mut(pointer).unwrap() = json!(false);
        });
        let read = db.begin_read().unwrap();
        assert!(
            !read
                .attempt_is_sent(sid(1), NodeId(2), key(tid(1), 0, 0))
                .unwrap()
        );
        assert!(
            read.attempt_is_sent(sid(1), NodeId(2), key(tid(1), 0, 1))
                .unwrap()
        );
        for other in [key(tid(2), 0, 1), key(tid(1), 1, 1), key(tid(1), 0, 2)] {
            assert!(!read.attempt_is_sent(sid(1), NodeId(2), other).unwrap());
        }
        assert_eq!(read.turn_agent(sid(1), tid(1)).unwrap(), "alpha");
        assert!(matches!(
            read.path_node(sid(1), node),
            Err(StoreError::Corruption { .. })
        ));
    }
}

fn edit_projection_node(db: &SessionStore, node: NodeId, edit: impl FnOnce(&mut Value)) {
    db.edit_test_node(
        NodeRef {
            session_id: sid(1),
            node,
        },
        edit,
    )
    .unwrap();
}

#[test]
fn usage_and_log_projections_skip_conversation_and_execution_material() {
    for position in [NodeId(2), NodeId(0)] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
            .unwrap();
        db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 0), 0)), now())
            .unwrap();
        db.apply_context_op(
            &lock,
            ApplyContextOp::new(
                sid(1),
                project(1),
                ContextOp::new(channel("tui"), vec![ContextEdit::omit_node(NodeId(1))]).unwrap(),
                now(),
            ),
        )
        .unwrap();
        db.rewind(&lock, position, &project(1), now()).unwrap();
        let expected_usage = db.begin_read().unwrap().usage(sid(1)).unwrap();
        let expected_log = db.begin_read().unwrap().log(sid(1)).unwrap();
        assert_eq!(
            expected_usage.path_total.attempts,
            u64::from(position != NodeId(0))
        );
        assert_eq!(expected_usage.session_total.attempts, 1);
        for (node, pointers) in [
            (NodeId(1), &["/input"][..]),
            (NodeId(3), &["/op"][..]),
            (
                NodeId(2),
                &[
                    "/reply",
                    "/slots",
                    "/turn_start/config_digest",
                    "/turn_start/limits",
                    "/attempts/0/origin",
                ][..],
            ),
        ] {
            edit_projection_node(&db, node, |wire| {
                for pointer in pointers {
                    *wire.pointer_mut(pointer).unwrap() = json!(false);
                }
            });
        }
        assert_eq!(db.begin_read().unwrap().log(sid(1)).unwrap(), expected_log);
        edit_projection_node(&db, NodeId(2), |wire| {
            wire["progress"] = json!(false);
            wire["attempts"][0]["progress"] = json!(false);
        });
        assert_eq!(
            db.begin_read().unwrap().usage(sid(1)).unwrap(),
            expected_usage
        );
    }
}

#[test]
fn usage_projection_rejects_damaged_metering_fields() {
    for (node, pointer) in [
        (NodeId(2), "/turn_start/provider"),
        (NodeId(2), "/turn_start/model"),
        (NodeId(2), "/attempts"),
        (NodeId(2), "/attempts/0/usage"),
        (NodeId(2), "/attempts/0/usage_kind"),
        (NodeId(2), "/attempts/0/cost"),
    ] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
            .unwrap();
        edit_projection_node(&db, node, |wire| {
            *wire.pointer_mut(pointer).unwrap() = json!(false);
        });
        assert!(
            matches!(
                db.begin_read().unwrap().usage(sid(1)),
                Err(StoreError::Corruption { .. })
            ),
            "node={node:?} pointer={pointer}"
        );
    }
}

#[test]
fn observer_projections_reject_damaged_fields() {
    for (pointer, check_agent) in [
        ("/kind", false),
        ("/turn_id", false),
        ("/step_ordinal", false),
        ("/attempts", false),
        ("/attempts/0/progress", false),
        ("/attempts/0/progress/progress", false),
        ("/kind", true),
        ("/turn_id", true),
        ("/step_ordinal", true),
        ("/turn_start", true),
        ("/turn_start/agent_name", true),
    ] {
        for replacement in [None, Some(json!(false))] {
            let (_d, _l, db) = store();
            let lock = db.try_lock_session(sid(1)).unwrap();
            accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
            db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
                .unwrap();
            edit_projection_node(&db, NodeId(2), |wire| {
                let (parent, field) = pointer.rsplit_once('/').unwrap();
                let object = wire.pointer_mut(parent).unwrap().as_object_mut().unwrap();
                if let Some(value) = replacement {
                    object.insert(field.into(), value);
                } else {
                    object.remove(field);
                }
            });
            let read = db.begin_read().unwrap();
            let result = if check_agent {
                read.turn_agent(sid(1), tid(1)).map(|_| ())
            } else {
                read.attempt_is_sent(sid(1), NodeId(2), key(tid(1), 0, 0))
                    .map(|_| ())
            };
            assert!(
                matches!(result, Err(StoreError::Corruption { table, key, .. })
                if table == "nodes" && key == format!("{}/2", sid(1))),
                "pointer={pointer} check_agent={check_agent}"
            );
        }
    }
    for (field, value, check_agent) in [
        ("kind", json!("input"), false),
        ("kind", json!("input"), true),
        ("turn_id", json!(tid(2)), true),
        ("step_ordinal", json!(1), true),
    ] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        edit_projection_node(&db, NodeId(2), |wire| wire[field] = value);
        let read = db.begin_read().unwrap();
        let result = if check_agent {
            read.turn_agent(sid(1), tid(1)).map(|_| ())
        } else {
            read.attempt_is_sent(sid(1), NodeId(2), key(tid(1), 0, 0))
                .map(|_| ())
        };
        assert!(matches!(result, Err(StoreError::Corruption { .. })));
    }
}
