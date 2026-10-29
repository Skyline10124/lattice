// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 前后端分离、回执丢失与按 TurnId 找回、身份冲突、Busy 与并发、视图推送与历史查询、
//! 取消与 stop 的真实二进制场景。提交与视图边界经公开 Local IPC，会话事实经公开 CLI 读取。
mod support;
use lattice_system_harness::{
    CancelRecord, CancelStatus, CommandOutcome, FAKE_API_KEY, LatticeCommand, RunningCommand,
    ScenarioBudget, ScenarioEvidence, ScenarioWorld, SendOutcome, SendRecord, SessionSendDriver,
    SseScript, StepProgress, merge_segments,
};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};
use support::{
    Watcher, Wire, assert_accepted_and_closed, assert_clean_send, assert_forced_kill,
    configuration, direct_world, driver, history, ipc, parse_send_stdout, submit_request,
    tool_call, turn_closed, turn_lookup, wire_fake_llm, wire_gated_fake_llm,
};
const FIRST: &str = "00000000-0000-4000-8000-000000000001";
const SECOND: &str = "00000000-0000-4000-8000-000000000002";
const THIRD: &str = "00000000-0000-4000-8000-000000000003";
/// 客户端为新会话生成的 SessionId（32 位小写十六进制）。
const NEW_SESSION: &str = "5e550000000000000000000000000001";
/// `watch` 开始时给出的轮数。
const WATCH_TURNS: usize = 10;
fn wait(budget: &ScenarioBudget) -> Duration {
    budget.cap_timeout(Duration::from_secs(30))
}
fn command(world: &ScenarioWorld, budget: &ScenarioBudget, args: &[&str]) -> RunningCommand {
    driver()
        .start(
            world,
            &LatticeCommand {
                args: args.iter().map(|s| (*s).into()).collect(),
                timeout: Some(wait(budget)),
            },
        )
        .unwrap()
}
fn run(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
) -> CommandOutcome {
    let out = command(world, budget, args).wait(None).unwrap();
    evidence.record_command(args.join(" "), &out);
    out
}
fn lines(out: &CommandOutcome) -> Vec<Value> {
    assert!(out.stderr.is_empty());
    std::str::from_utf8(&out.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
fn records(out: &CommandOutcome) -> SendOutcome {
    let parsed = parse_send_stdout(&out.stdout, out.exit_code());
    assert!(
        parsed.malformed_lines.is_empty(),
        "{:?}",
        parsed.malformed_lines
    );
    parsed
}
/// CLI 生成并经接受回执确认的 TurnId。
fn accepted_turn(out: &CommandOutcome) -> String {
    records(out)
        .accepted_turn_id()
        .expect("accepted TurnId")
        .to_owned()
}
fn read(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
) -> Value {
    let out = run(world, budget, evidence, args);
    assert!(out.is_clean_exit(0), "{}", out.summary());
    serde_json::from_slice(&out.stdout).unwrap()
}
fn detail(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    turn: &str,
) -> Value {
    read(
        world,
        budget,
        evidence,
        &["--json", "session", "turn", turn],
    )
}
fn closed(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    turn: &str,
) -> Value {
    let deadline = Instant::now() + wait(budget);
    loop {
        let t = detail(world, budget, evidence, turn);
        if t["summary"]["progress"]["progress"] == "closed" {
            return t;
        }
        assert!(Instant::now() < deadline, "Turn did not close");
        std::thread::yield_now();
    }
}
/// 正在执行的各轮：CLI 生成的身份可从会话列表中未关闭的最后一轮读出。
fn running_turns(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
) -> Vec<(String, String)> {
    read(world, budget, evidence, &["--json", "session", "list"])
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| {
            Some((
                row["session_id"].as_str()?.to_owned(),
                row["unclosed_turn"].as_str()?.to_owned(),
            ))
        })
        .collect()
}
/// IPC 错误回复中的冲突原因。
fn conflict(response: &Value) -> Option<&str> {
    (response["response"] == "error" && response["value"]["kind"] == "conflict")
        .then(|| response["value"]["conflict"].as_str())
        .flatten()
}
fn archive_state(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    turns: &[&str],
) {
    let list = read(world, budget, evidence, &["--json", "session", "list"]);
    let mut records = Vec::new();
    for turn in turns {
        let record = detail(world, budget, evidence, turn);
        let sid = record["session_id"].as_str().unwrap();
        read(world, budget, evidence, &["--json", "session", "log", sid]);
        let tree = run(world, budget, evidence, &["session", "log", sid]);
        assert!(tree.is_clean_exit(0));
        read(
            world,
            budget,
            evidence,
            &[
                "--json",
                "audit",
                "replay",
                "--session",
                sid,
                "--turn",
                turn,
            ],
        );
        records.push(record);
    }
    read(
        world,
        budget,
        evidence,
        &["--json", "audit", "--tail", "--since", "0"],
    );
    read(world, budget, evidence, &["--json", "lancet", "status"]);
    evidence
        .record_json("state-summary", &json!({"list":list,"turns":records}))
        .unwrap();
}
fn finish(evidence: ScenarioEvidence, result: std::thread::Result<()>) {
    let primary = result.err().map(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "non-text panic".into())
    });
    evidence.finish_report(primary).unwrap();
}
#[test]
fn detached_cli_finishes_and_world_owns_daemon_cleanup() {
    let (world, spec) = direct_world("background-cli-detach", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (server, gate) = wire_gated_fake_llm(
            &world,
            "background",
            SseScript::plain("detached durable answer"),
        );
        let front = command(
            &world,
            &budget,
            &["--json", "run", "keep running", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        let running = running_turns(&world, &budget, &mut evidence);
        assert_eq!(running.len(), 1);
        let (sid, tid) = running[0].clone();
        // 另起的客户端按 TurnId 查询：已接受，属于同一会话。
        let found = turn_lookup(&world, wait(&budget), &tid);
        assert_eq!(found["response"], "turn", "{found}");
        assert_eq!(found["value"]["status"], "accepted");
        assert_eq!(found["value"]["turn"]["session_id"], sid);
        let owners = world.daemon_pids().unwrap();
        assert_eq!(owners.len(), 1);
        assert_ne!(owners[0], front.pgid());
        let killed = front.kill_and_wait().unwrap();
        evidence.record_command("detach CLI", &killed);
        assert_forced_kill(&killed);
        assert_eq!(world.daemon_pids().unwrap(), owners);
        assert_ne!(
            detail(&world, &budget, &mut evidence, &tid)["summary"]["progress"]["progress"],
            "closed"
        );
        gate.release(wait(&budget)).unwrap();
        assert_eq!(
            closed(&world, &budget, &mut evidence, &tid)["summary"]["outcome"],
            "completed"
        );
        let observed = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "turn", &tid, "--follow"],
        );
        assert!(observed.is_clean_exit(0), "{}", observed.summary());
        let followed = records(&observed);
        assert_eq!(
            followed.closed_result(),
            Some((sid.as_str(), tid.as_str(), "completed"))
        );
        assert_eq!(followed.reply_text(), "detached durable answer");
        assert!(followed.entries().iter().all(|entry| entry.turn_id == tid));
        assert_eq!(server.captured_requests().len(), 1);
        evidence.record_captures(server.captured_requests());
        archive_state(&world, &budget, &mut evidence, &[&tid]);
        evidence.record_json("parameters", &json!({"turn":tid,"script":"gated plain detached durable answer","frontend":"SIGKILL after acceptance","backend_pids":owners})).unwrap();
        world.stop_daemon().unwrap();
        for pid in owners {
            assert!(
                lattice_system_harness::processes_in_pgid(pid)
                    .unwrap()
                    .is_empty()
            );
        }
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn unread_acceptance_is_found_by_turn_id_and_resubmission_conflicts() {
    for existing_session in [false, true] {
        for restart in [false, true] {
            let (world, spec) = direct_world(
                &format!("background-unread-{existing_session}-{restart}"),
                "background",
                "[]",
                "",
            );
            let budget = world.budget(&spec);
            let mut evidence = support::evidence(&world, &spec);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let existing = existing_session.then(|| {
                    let mut seed =
                        wire_fake_llm(&world, "background", SseScript::plain("seed reply"));
                    let out = run(
                        &world,
                        &budget,
                        &mut evidence,
                        &["--json", "run", "seed input", "--agent", "background"],
                    );
                    assert!(out.is_clean_exit(0));
                    evidence
                        .record_json("seed-provider", &json!(seed.captured_requests()))
                        .unwrap();
                    seed.shutdown(wait(&budget)).unwrap();
                    records(&out).accepted_session_id().unwrap().to_owned()
                });
                let (mut server, gate) = wire_gated_fake_llm(
                    &world,
                    "background",
                    SseScript::plain("one request one Turn"),
                );
                let bootstrap = run(
                    &world,
                    &budget,
                    &mut evidence,
                    &["--json", "session", "list"],
                );
                assert!(bootstrap.is_clean_exit(if existing_session { 0 } else { 1 }));
                let sid = existing.clone().unwrap_or_else(|| NEW_SESSION.into());
                let session = if existing_session {
                    json!({"existing": sid})
                } else {
                    json!({"new": sid})
                };
                let request = submit_request(
                    &world,
                    FIRST,
                    session,
                    "identical input",
                    Some("background"),
                    configuration(&world, "background", &server.base_url()),
                );
                // 先用只读的 `status` 确认后台说同一协议，再进入未读回执窗口；
                // 协议不符时在这里直接失败，而不是表现为 provider 一直等不到请求。
                let status = ipc(&world, wait(&budget), json!({"operation":"status"}));
                assert_eq!(status["response"], "status", "{status}");
                let unread = Wire::connect(&world, wait(&budget), &request);
                gate.wait_until_stalled(1, wait(&budget)).unwrap();
                // 此连接只写请求，从未读取回执；按 TurnId 查询得到已接受。
                let found = turn_lookup(&world, wait(&budget), FIRST);
                assert_eq!(found["value"]["status"], "accepted", "{found}");
                assert_eq!(found["value"]["turn"]["session_id"], sid);
                let pending = detail(&world, &budget, &mut evidence, FIRST);
                assert_ne!(pending["summary"]["progress"]["progress"], "closed");
                assert_eq!(pending["session_id"], sid);
                drop(unread);
                let epoch =
                    ipc(&world, wait(&budget), json!({"operation":"status"}))["value"]["epoch"]
                        .clone();
                evidence.record_json("unread-receipt-window", &json!({"existing_session":existing_session,
                    "restart":restart,"pending":pending,"lookup":found,"epoch":epoch,"request_id":FIRST,
                    "input":"identical input","receipt_read":false,"provider_arrivals":1})).unwrap();
                if !restart {
                    // Owner 仍在执行时以同一身份重新提交：TurnId 已被使用，不再兼做找回。
                    let retry = ipc(&world, wait(&budget), request.clone());
                    assert_eq!(conflict(&retry), Some("turn_id_used"), "{retry}");
                    evidence.record_json("running-retry", &retry).unwrap();
                }
                gate.release(wait(&budget)).unwrap();
                let original = closed(&world, &budget, &mut evidence, FIRST);
                if restart {
                    world.stop_daemon().unwrap();
                }
                // 后台持续运行或重启后，同一 TurnId 都查到同一个已关闭的 Turn。
                assert_eq!(detail(&world, &budget, &mut evidence, FIRST), original);
                let reread = turn_lookup(&world, wait(&budget), FIRST);
                assert_eq!(reread["value"]["status"], "accepted", "{reread}");
                assert_eq!(reread["value"]["turn"], original);
                let current_epoch =
                    ipc(&world, wait(&budget), json!({"operation":"status"}))["value"]["epoch"]
                        .clone();
                assert_eq!(epoch != current_epoch, restart);
                // 新建会话时 TurnId 先于会话核对，得到的是 TurnId 冲突而不是会话已存在。
                let repeat = ipc(&world, wait(&budget), request.clone());
                assert_eq!(conflict(&repeat), Some("turn_id_used"), "{repeat}");
                assert_eq!(server.captured_requests().len(), 1);
                let list = read(
                    &world,
                    &budget,
                    &mut evidence,
                    &["--json", "session", "list"],
                );
                assert_eq!(list.as_array().unwrap().len(), 1);
                assert_eq!(list[0]["session_id"], sid);
                assert_eq!(list[0]["node_count"], if existing_session { 5 } else { 3 });
                evidence.record_json("identity-retry", &json!({"original":original,"epoch_after":current_epoch,"conflict":repeat,"list":list})).unwrap();
                let independent = run(
                    &world,
                    &budget,
                    &mut evidence,
                    &["--json", "run", "identical input", "--session", &sid],
                );
                assert!(independent.is_clean_exit(0), "{}", independent.summary());
                let next = accepted_turn(&independent);
                assert_ne!(next, FIRST);
                assert_eq!(server.captured_requests().len(), 2);
                archive_state(&world, &budget, &mut evidence, &[FIRST, &next]);
                evidence
                    .record_json(
                        "parameters",
                        &json!({"existing_session":existing_session,"restart":restart,
                    "script":"one request one Turn","state_root":world.session_store_dir().parent(),
                    "provider_requests":2,"backend":world.daemon_pids().unwrap()}),
                    )
                    .unwrap();
                evidence.record_captures(server.captured_requests());
                server.shutdown(wait(&budget)).unwrap();
            }));
            support::finish_recorded(evidence, result);
        }
    }
}
#[test]
fn busy_cancel_and_stop_do_not_serialize_distinct_sessions() {
    let (world, spec) = direct_world("background-concurrent-stop", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (server, gate) =
            wire_gated_fake_llm(&world, "background", SseScript::plain("parallel answer"));
        let first = command(
            &world,
            &budget,
            &["--json", "run", "parallel a", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        let (a, first_turn) = running_turns(&world, &budget, &mut evidence)[0].clone();
        let first_detail = detail(&world, &budget, &mut evidence, &first_turn);
        let second = command(
            &world,
            &budget,
            &["--json", "run", "parallel b", "--agent", "background"],
        );
        gate.wait_until_stalled(2, wait(&budget)).unwrap();
        let (b, second_turn) = running_turns(&world, &budget, &mut evidence)
            .into_iter()
            .find(|(sid, _)| *sid != a)
            .expect("second Session");
        let config = configuration(&world, "background", &server.base_url());
        let busy = ipc(
            &world,
            wait(&budget),
            submit_request(
                &world,
                THIRD,
                json!({"existing": a}),
                "must not queue",
                None,
                config.clone(),
            ),
        );
        assert_eq!(busy["value"]["kind"], "busy", "{busy}");
        let second_detail = detail(&world, &budget, &mut evidence, &second_turn);
        evidence
            .record_json(
                "concurrent-window",
                &json!({"provider_arrivals":server.captured_requests().len(),
            "first":first_detail,"second":second_detail,"busy":busy,
            "backend":world.daemon_pids().unwrap(),"gate_released":false}),
            )
            .unwrap();
        // 另一个终端按会话取消：CLI 从会话头查出正在执行的那一轮再按 TurnId 取消。
        let cancel = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "cancel", &a],
        );
        assert!(cancel.is_clean_exit(0), "{}", cancel.summary());
        let cancelled = records(&cancel);
        assert_eq!(
            cancelled.records,
            vec![SendRecord::Cancel(CancelRecord::Reply {
                session_id: Some(a.clone()),
                turn_id: first_turn.clone(),
                status: CancelStatus::Taken,
                owned: Some(true),
                error: None,
            })]
        );
        let detached = first.kill_and_wait().unwrap();
        evidence.record_command("detach after Taken", &detached);
        assert_forced_kill(&detached);
        assert_eq!(
            closed(&world, &budget, &mut evidence, &first_turn)["summary"]["outcome"],
            "cancelled"
        );
        // 持久关闭先于监督者移除 Owner；无可取消的断言必须等到后台完成收尾。
        let deadline = Instant::now() + wait(&budget);
        let mut watcher = Watcher::open(&world, wait(&budget), &a);
        watcher.until(
            deadline,
            "the cancelled turn's Owner to be removed",
            |view| {
                view.head.as_ref().is_some_and(|head| {
                    !head.owner && head.turn_id.as_deref() == Some(first_turn.as_str())
                })
            },
        );
        // 另一个会话的那一轮不受影响；这个会话已没有正在执行的一轮。
        assert_ne!(
            detail(&world, &budget, &mut evidence, &second_turn)["summary"]["progress"]["progress"],
            "closed"
        );
        let nothing = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "cancel", &a],
        );
        assert!(nothing.is_clean_exit(0), "{}", nothing.summary());
        assert_eq!(
            records(&nothing).records,
            vec![SendRecord::Cancel(CancelRecord::NothingToCancel {
                session_id: a.clone()
            })]
        );
        let mut stop = command(&world, &budget, &["--json", "daemon", "stop"]);
        let deadline = Instant::now() + wait(&budget);
        loop {
            if ipc(&world, wait(&budget), json!({"operation":"status"}))["value"]["stopping"]
                == true
            {
                break;
            }
            assert!(Instant::now() < deadline);
        }
        assert_eq!(
            ipc(
                &world,
                wait(&budget),
                submit_request(
                    &world,
                    THIRD,
                    json!({"existing": a}),
                    "after stop",
                    None,
                    config
                )
            )["value"]["kind"],
            "stopping"
        );
        assert_ne!(
            detail(&world, &budget, &mut evidence, &second_turn)["summary"]["progress"]["progress"],
            "closed"
        );
        // stop 必须等待被 gate 挡住的在途 Turn，不能在释放前就返回。
        assert!(
            !stop.has_exited().unwrap(),
            "daemon stop returned while taken work was still in flight"
        );
        gate.release(wait(&budget)).unwrap();
        let done = second.wait(None).unwrap();
        evidence.record_command("parallel completion", &done);
        if done.is_clean_exit(0) {
            assert_eq!(records(&done).reply_text(), "parallel answer");
        } else {
            assert!(done.is_clean_exit(1), "{}", done.summary());
            assert!(records(&done).records.iter().any(|record| matches!(
                record,
                SendRecord::Error(_) | SendRecord::Connection { .. }
            )));
        }
        let drained = stop.wait(None).unwrap();
        evidence.record_command("stop drains", &drained);
        assert!(drained.is_clean_exit(0));
        assert_eq!(lines(&drained)[0]["owners"], 0);
        world.stop_daemon().unwrap();
        assert_eq!(
            detail(&world, &budget, &mut evidence, &second_turn)["summary"]["outcome"],
            "completed"
        );
        assert_eq!(server.captured_requests().len(), 2);
        evidence.record_captures(server.captured_requests());
        archive_state(&world, &budget, &mut evidence, &[&first_turn, &second_turn]);
        evidence.record_json("parameters", &json!({"script":"parallel answer","requests":["parallel a","parallel b"],
            "sessions":[a,b],"state_root":world.session_store_dir().parent(),"provider_requests":2,"stop":lines(&drained)})).unwrap();
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn concurrent_autostart_has_one_owner_and_midway_watch_matches_history() {
    let (world, spec) = direct_world("background-start-watch", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let large = "大".repeat(24000);
        let (server, gate) = wire_gated_fake_llm(
            &world,
            "background",
            support::sequence(vec![
                SseScript::plain(large.clone()),
                SseScript::plain(large.clone()),
                SseScript::plain("after"),
            ]),
        );
        world.stop_daemon().unwrap();
        assert!(
            world.daemon_pids().unwrap().is_empty(),
            "concurrent autostart requires no running daemon"
        );
        let a = command(
            &world,
            &budget,
            &["--json", "run", "large a", "--agent", "background"],
        );
        let b = command(
            &world,
            &budget,
            &["--json", "run", "large b", "--agent", "background"],
        );
        gate.wait_until_stalled(2, wait(&budget)).unwrap();
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        let running = running_turns(&world, &budget, &mut evidence);
        assert_eq!(running.len(), 2);
        let (sid, tid) = running[0].clone();
        // 中途开始的观察：会话头说明当前位置那一轮有 Owner，随后是这一轮的条目。
        let mut watcher = Watcher::open(&world, wait(&budget), &sid);
        let view = watcher.initial();
        let head = view.head.clone().unwrap();
        assert!(head.owner && !head.path_replaced, "{head:?}");
        assert_eq!(head.turn_id.as_deref(), Some(tid.as_str()));
        let entries: Vec<_> = view.entries.values().collect();
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[0].user_ordinal(), Some(1));
        assert_eq!(entries[1].node, head.position);
        assert!(!entries[1].step().unwrap().progress.is_closed());
        let midway = watcher.received.clone();
        gate.release(wait(&budget)).unwrap();
        for front in [a, b] {
            let done = front.wait(None).unwrap();
            evidence.record_command("concurrent first send", &done);
            assert!(done.is_clean_exit(0), "{}", done.summary());
            let sent = records(&done);
            assert_accepted_and_closed(&sent).unwrap();
            assert_eq!(sent.reply_text(), large);
        }
        watcher.until(
            Instant::now() + wait(&budget),
            "the midway-watched Turn closes",
            |view| turn_closed(view, &tid),
        );
        let held: Vec<Value> = watcher.view.raw_entries().into_iter().cloned().collect();
        // 关闭后在同一会话再提交一轮，用历史查询重读它之前的那一轮。
        let after = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "after", "--session", &sid],
        );
        assert!(after.is_clean_exit(0), "{}", after.summary());
        let next_user = records(&after).entries()[0].node;
        let reread = history(&world, wait(&budget), &sid, next_user, 1).unwrap();
        assert_eq!(reread, held);
        assert_eq!(server.captured_requests().len(), 3);
        archive_state(&world, &budget, &mut evidence, &[&tid]);
        evidence
            .record_json(
                "parameters",
                &json!({"script":"gated 24000 Chinese characters","turns":running,
            "midway":midway,"held":held,"history":reread}),
            )
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn slow_watcher_cannot_block_execution_and_converges_to_history() {
    let (world, spec) = direct_world("background-slow-watcher", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let chunk = "slow-reader-".repeat(100);
        let text = chunk.repeat(1600);
        let (server, gate) = wire_gated_fake_llm(
            &world,
            "background",
            support::sequence(vec![
                SseScript::PlainText {
                    chunks: vec![chunk; 1600],
                    finish_reason: "stop".into(),
                },
                SseScript::plain("next answer"),
            ]),
        );
        let front = command(
            &world,
            &budget,
            &["--json", "run", "slow observer", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        let (sid, tid) = running_turns(&world, &budget, &mut evidence)[0].clone();
        let mut slow = Watcher::open(&world, wait(&budget), &sid);
        slow.initial();
        // 观察者此后不再读取；后台执行与其他查询不受它影响。
        gate.release(wait(&budget)).unwrap();
        let done = front.wait(None).unwrap();
        evidence.record_command("normal beside slow", &done);
        assert!(done.is_clean_exit(0), "{}", done.summary());
        assert_eq!(records(&done).reply_text(), text);
        assert_eq!(
            closed(&world, &budget, &mut evidence, &tid)["summary"]["outcome"],
            "completed"
        );
        // 慢观察者只少收中间版本：赶上后持有每个条目的最新值，同一节点不回退，
        // 收到过的生成中文字只是最终正文的前缀。
        slow.until(
            Instant::now() + wait(&budget),
            "the slow watcher holds the closed Turn",
            |view| turn_closed(view, &tid),
        );
        let held: Vec<Value> = slow.view.raw_entries().into_iter().cloned().collect();
        for segments in slow.view.texts.values() {
            let merged = merge_segments(segments);
            assert!(merged.len() <= 1, "{merged:?}");
            assert!(merged.iter().all(|segment| text.starts_with(&segment.text)));
        }
        let next = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "next", "--session", &sid],
        );
        assert!(next.is_clean_exit(0), "{}", next.summary());
        let next_user = records(&next).entries()[0].node;
        assert_eq!(
            history(&world, wait(&budget), &sid, next_user, 1).unwrap(),
            held
        );
        assert_eq!(server.captured_requests().len(), 2);
        archive_state(&world, &budget, &mut evidence, &[&tid]);
        evidence
            .record_json(
                "parameters",
                &json!({"script":"slow-reader- repeated 160000 times","turn":tid,
                    "slow_received":slow.received.len()}),
            )
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn shared_daemon_uses_registered_providers_and_freezes_profiles() {
    use lattice_system_harness::{FakeLlmServer, assert_no_secret};
    let (world, spec) = direct_world(
        "background-project-config",
        "background",
        "[\"session__todo_write\"]",
        "",
    );
    let budget = world.budget(&spec);
    let key_a = "project-alpha-private-fixture-key";
    let key_b = "project-beta-private-fixture-key";
    let mut evidence =
        ScenarioEvidence::new(&world, &spec.name, [key_a.into(), key_b.into()]).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let script = SseScript::Sequence {
            scripts: vec![
                tool_call(
                    "todo",
                    "session__todo_write",
                    r#"{"todos":[{"content":"isolation","status":"pending"}]}"#,
                ),
                SseScript::plain("frozen result"),
            ],
        };
        let (a, ga) = FakeLlmServer::start_gated(script.clone()).unwrap();
        let (b, gb) = FakeLlmServer::start_gated(script).unwrap();
        a.expect_api_key(key_a);
        b.expect_api_key(key_b);
        use lattice_system_harness::{ProviderFixture, ProviderProtocol, ProviderTarget};
        let alpha = ProviderFixture {
            protocol: ProviderProtocol::ChatCompletions,
            target: ProviderTarget::Custom("alpha"),
            model: "background",
        };
        let beta = ProviderFixture {
            target: ProviderTarget::Custom("beta"),
            ..alpha
        };
        world.wire_provider(alpha, &a, key_a).unwrap();
        world.wire_provider(beta, &b, key_b).unwrap();
        let project = world.root().join("second-project");
        std::fs::create_dir_all(project.join(".lattice/agents/background")).unwrap();
        let profile = world
            .project_dir()
            .join(".lattice/agents/background/agent.toml");
        std::fs::write(
            &profile,
            std::fs::read_to_string(&profile)
                .unwrap()
                .replace("local/background", "alpha/background"),
        )
        .unwrap();
        std::fs::write(
            project.join(".lattice/agents/background/agent.toml"),
            std::fs::read_to_string(&profile)
                .unwrap()
                .replace("concise test agent", "beta test agent")
                .replace("alpha/background", "beta/background"),
        )
        .unwrap();
        gix::init(&project).unwrap();
        let first = command(
            &world,
            &budget,
            &["--json", "run", "alpha", "--agent", "background"],
        );
        ga.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let mut cmd = world.command(driver().binary_path());
        cmd.current_dir(&project)
            .args(["--json", "run", "beta", "--agent", "background"]);
        let second = RunningCommand::spawn(cmd).unwrap();
        gb.wait_until_stalled(1, budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        std::fs::write(profile, "invalid changed profile").unwrap();
        ga.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        gb.release(budget.cap_timeout(Duration::from_secs(30)))
            .unwrap();
        let first = first.wait(None).unwrap();
        let second = second
            .wait(Some(budget.cap_timeout(Duration::from_secs(30))))
            .unwrap();
        for (name, out) in [("alpha", &first), ("beta", &second)] {
            evidence.record_command(name, out);
            assert!(out.is_clean_exit(0), "{name}: {}", out.summary());
            assert_no_secret(out, key_a).unwrap();
            assert_no_secret(out, key_b).unwrap();
        }
        let alpha_turn = accepted_turn(&first);
        let beta_turn = accepted_turn(&second);
        for (server, prompt) in [
            (&a, "You are a concise test agent."),
            (&b, "You are a beta test agent."),
        ] {
            let captures = server.captured_requests();
            assert_eq!(captures.len(), 2);
            for c in &captures {
                assert_eq!(c.raw["messages"][0]["content"], prompt);
            }
            evidence.record_captures(captures);
        }
        archive_state(&world, &budget, &mut evidence, &[&alpha_turn]);
        let beta = detail(&world, &budget, &mut evidence, &beta_turn);
        let sid = beta["session_id"].as_str().unwrap();
        for args in [
            vec!["--json", "session", "log", sid],
            vec!["session", "log", sid],
            vec!["--json", "session", "turn", &beta_turn],
            vec!["--json", "audit", "--tail"],
            vec![
                "--json",
                "audit",
                "replay",
                "--session",
                sid,
                "--turn",
                &beta_turn,
            ],
            vec!["--json", "lancet", "status"],
        ] {
            let mut cmd = world.command(driver().binary_path());
            cmd.current_dir(&project).args(&args);
            let out = RunningCommand::spawn(cmd)
                .unwrap()
                .wait(Some(budget.cap_timeout(Duration::from_secs(30))))
                .unwrap();
            evidence.record_command(format!("beta {}", args.join(" ")), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
        }
        evidence.record_json("beta-state", &beta).unwrap();
        evidence.record_json("parameters", &json!({"turns":[alpha_turn,beta_turn],"prompts":["alpha","beta"],"methods":["session__todo_write"],"final_reply":"frozen result","provider_requests":[2,2],"source_changed_after_both_gates":true})).unwrap();
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn later_session_driver_keeps_the_scenario_budget_origin() {
    let (world, spec) = direct_world("background-budget-origin", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(run(&world, &budget, &mut evidence, &["--help"]).is_clean_exit(0));
        let consumed = budget.elapsed();
        let creating = Instant::now();
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        let elapsed = creating.elapsed();
        assert!(send.budget().elapsed() >= consumed + elapsed);
        assert_eq!(
            send.budget().remaining().is_some(),
            budget.remaining().is_some()
        );
    }));
    finish(evidence, result);
}
#[test]
fn exhausted_budget_records_normal_stop_failure_without_fixed_wait() {
    let (world, mut spec) = direct_world("background-budget-exhausted", "background", "[]", "");
    spec.total_timeout_secs = Some(0);
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(budget.remaining(), Some(Duration::ZERO));
        let root = world
            .session_store_dir()
            .parent()
            .unwrap()
            .display()
            .to_string();
        let daemon = driver()
            .start(
                &world,
                &LatticeCommand {
                    args: ["daemon", "serve", "--state-root", &root]
                        .iter()
                        .map(|s| (*s).into())
                        .collect(),
                    timeout: Some(spec.step_timeout()),
                },
            )
            .unwrap();
        let deadline = Instant::now() + spec.step_timeout();
        while world.daemon_pids().unwrap() != vec![daemon.pgid()] {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let start = Instant::now();
        let errors = world
            .stop_daemon()
            .expect_err("normal stop must exhaust budget");
        assert!(errors.iter().any(|s| s.contains("budget")));
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(world.daemon_pids().unwrap().is_empty());
        let killed = daemon.wait(None).unwrap();
        evidence.record_command("exhausted budget cleanup", &killed);
        assert!(killed.cleanup.surviving_pids.is_empty());
    }));
    finish(evidence, result);
}
#[test]
fn daemon_restart_reads_closed_result_without_resubmitting() {
    let (world, spec) = direct_world(
        "background-restart-read",
        "background",
        "[\"session__todo_write\"]",
        "",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let server = wire_fake_llm(
            &world,
            "background",
            SseScript::Sequence {
                scripts: vec![
                    tool_call(
                        "todo",
                        "session__todo_write",
                        r#"{"todos":[{"content":"durable","status":"pending"}]}"#,
                    ),
                    SseScript::plain("restart answer"),
                    SseScript::plain("answer 中"),
                ],
            },
        );
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        let sent = send.send("restart prompt").unwrap();
        evidence.record_command("before restart", &sent.command);
        assert_clean_send(&sent).unwrap();
        let tid = sent.accepted_turn_id().unwrap();
        let tool_session = send.session_id().unwrap();
        let original = detail(&world, &budget, &mut evidence, tid);
        // 同项目另建无 Tool 会话，保留空 replay 和单 Step 条目的独立信号。
        let plain = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "session",
                "send",
                "question",
                "--agent",
                "background",
            ],
        );
        assert!(plain.is_clean_exit(0), "{}", plain.summary());
        let plain_turn = accepted_turn(&plain);
        let plain_before = detail(&world, &budget, &mut evidence, &plain_turn);
        let sid = plain_before["session_id"].as_str().unwrap();
        world.stop_daemon().unwrap();
        let projects: Vec<_> = std::fs::read_dir(world.session_store_dir().join("projects"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(projects.len(), 1);
        let history_path = projects[0].join("workspace-history.git");
        std::fs::remove_dir_all(&history_path).unwrap();
        let sentinel = "deliberately invalid; closure must not read";
        std::fs::write(&history_path, sentinel).unwrap();
        let key = projects[0].file_name().unwrap();
        let lock_path = world
            .session_store_dir()
            .join("locks/projects")
            .join(key)
            .join("workspace.lock");
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let workspace_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(workspace_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        world
            .register_key(
                lattice_system_harness::ProviderFixture::chat_completions("background"),
                None,
            )
            .unwrap();
        assert_eq!(detail(&world, &budget, &mut evidence, tid), original);
        // 重启后重新打开会话：会话头说明没有 Owner，条目给出这一轮的全部节点。
        let mut observer = Watcher::open(&world, wait(&budget), tool_session);
        let view = observer.initial();
        let head = view.head.clone().unwrap();
        assert!(!head.owner && !head.path_replaced, "{head:?}");
        assert_eq!(head.turn_id.as_deref(), Some(tid));
        let entries: Vec<_> = view.entries.values().collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .iter()
                .filter_map(|entry| entry.step())
                .map(|step| step.ordinal)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(entries[2].raw.to_string().contains("restart answer"));
        let steps: Vec<&Value> = entries[1..]
            .iter()
            .map(|entry| &entry.raw["step"])
            .collect();
        assert_eq!(
            steps,
            original["steps"]
                .as_array()
                .unwrap()
                .iter()
                .collect::<Vec<_>>()
        );
        assert_eq!(entries[2].raw["summary"], original["summary"]);
        let mut plain_observer = Watcher::open(&world, wait(&budget), sid);
        let plain_view = plain_observer.initial();
        let plain_entries: Vec<Value> = plain_view.raw_entries().into_iter().cloned().collect();
        assert_eq!(plain_entries.len(), 2);
        assert_eq!(plain_entries[1]["node"], 2);
        assert_eq!(plain_entries[1]["content"]["kind"], "step");
        assert_eq!(plain_entries[1]["summary"]["outcome"], "completed");
        assert!(
            plain_entries[1]["content"]["reply"]["parts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|part| part["semantic"]["text"]["text"] == "answer 中")
        );
        // 历史查询只取 `before` 之前的轮：第 1 轮之前没有条目，`before` 不是 UserMessage 时拒绝。
        assert_eq!(history(&world, wait(&budget), sid, 1, 10), Ok(Vec::new()));
        let not_user = history(&world, wait(&budget), sid, 2, 10).unwrap_err();
        assert_eq!(not_user["value"]["kind"], "bad_request", "{not_user}");
        let followed = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "turn", &plain_turn, "--follow"],
        );
        assert!(followed.is_clean_exit(0), "{}", followed.summary());
        let delivered = records(&followed);
        assert_eq!(
            delivered
                .entries()
                .into_iter()
                .map(|entry| entry.raw.clone())
                .collect::<Vec<_>>(),
            plain_entries
        );
        let rewound = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "rewind", sid, "0"],
        );
        assert_eq!(rewound["current"], 0);
        // 空闲时退回：已打开的观察者收到标明路径已替换的会话头，此前的条目作废。
        let replaced = plain_observer.until(
            Instant::now() + wait(&budget),
            "a path-replaced head at the root",
            |view| {
                view.head
                    .as_ref()
                    .is_some_and(|head| head.path_replaced && head.position == 0)
            },
        );
        assert!(replaced.entries.is_empty());
        assert_eq!(
            detail(&world, &budget, &mut evidence, &plain_turn),
            plain_before
        );
        let log = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(log["nodes"].as_array().unwrap().len(), 3);
        let replay = read(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "audit",
                "replay",
                "--session",
                sid,
                "--turn",
                &plain_turn,
            ],
        );
        assert_eq!(replay["calls"], json!([]));
        assert_eq!(std::fs::read_to_string(&history_path).unwrap(), sentinel);
        evidence
            .record_json(
                "readonly-with-invalid-history",
                &json!({"watch":plain_observer.received,"rewind":rewound,"log":log,"replay":replay}),
            )
            .unwrap();
        assert_eq!(server.captured_requests().len(), 3);
        evidence
            .record_json("reopened-watch", &json!(observer.received))
            .unwrap();
        archive_state(&world, &budget, &mut evidence, &[tid]);
        evidence
            .record_json(
                "parameters",
                &json!({"script":"todo durable then restart answer","turn":tid,"restart":true}),
            )
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn a_live_new_turn_does_not_block_old_closed_observation() {
    let (world, spec) = direct_world("background-read-beside-live", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (server, gate) = wire_gated_fake_llm(&world, "background", SseScript::plain("answer"));
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        let first = send.spawn("first").unwrap();
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        gate.release_next(wait(&budget)).unwrap();
        let first = send.wait_spawned(first).unwrap();
        evidence.record_command("first", &first.command);
        assert_clean_send(&first).unwrap();
        assert!(first.has_completed());
        let tid = first.accepted_turn_id().unwrap();
        let old = detail(&world, &budget, &mut evidence, tid);
        // 重启清除旧 Turn 的 Owner，随后四个带配置的新身份共同争抢空闲 Session。
        world.stop_daemon().unwrap();
        detail(&world, &budget, &mut evidence, tid);
        let sid = old["session_id"].as_str().unwrap();
        let config = configuration(&world, "background", &server.base_url());
        let candidates = [
            SECOND,
            THIRD,
            "00000000-0000-4000-8000-000000000004",
            "00000000-0000-4000-8000-000000000005",
        ];
        let barrier = std::sync::Barrier::new(candidates.len());
        let submissions = std::thread::scope(|scope| {
            let handles: Vec<_> = candidates
                .iter()
                .map(|candidate| {
                    let request = submit_request(
                        &world,
                        candidate,
                        json!({"existing": sid}),
                        "second",
                        None,
                        config.clone(),
                    );
                    let world = &world;
                    let budget = &budget;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        ipc(world, wait(budget), request)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        evidence
            .record_json("concurrent-admission", &json!(submissions))
            .unwrap();
        let mut winners = Vec::new();
        let mut rejected = Vec::new();
        for (candidate, response) in candidates.iter().zip(&submissions) {
            if response["response"] == "acceptance" && response["value"]["acceptance"] == "accepted"
            {
                winners.push(*candidate);
            } else {
                assert_eq!(response["value"]["kind"], "busy", "{response}");
                rejected.push(*candidate);
            }
        }
        assert_eq!((winners.len(), rejected.len()), (1, 3));
        for candidate in rejected {
            let absent = turn_lookup(&world, wait(&budget), candidate);
            assert_eq!(absent["value"]["kind"], "not_found", "{absent}");
        }
        gate.wait_until_stalled(2, wait(&budget)).unwrap();
        let blocked = ipc(
            &world,
            wait(&budget),
            json!({"operation":"rewind","session_id":sid,
            "target":0,"workspace":world.project_dir()}),
        );
        assert_eq!(blocked["value"]["kind"], "busy", "{blocked}");
        // 已用的 TurnId 先于 Busy 核对：同一身份重新提交得到 Conflict，不是找回。
        let reused = ipc(
            &world,
            wait(&budget),
            submit_request(
                &world,
                tid,
                json!({"existing": sid}),
                "first",
                None,
                config.clone(),
            ),
        );
        assert_eq!(conflict(&reused), Some("turn_id_used"), "{reused}");
        evidence
            .record_json(
                "used-identity-beside-live",
                &json!({"rewind":blocked,"resubmit":reused}),
            )
            .unwrap();
        // 会话头直接说明当前位置那一轮（新一轮）有 Owner；旧一轮按 TurnId 查到已关闭。
        let mut watcher = Watcher::open(&world, wait(&budget), sid);
        let head = watcher.initial().head.clone().unwrap();
        assert_eq!(head.turn_id.as_deref(), Some(winners[0]));
        assert!(head.owner);
        let before = turn_lookup(&world, wait(&budget), tid);
        assert_eq!(before["value"]["turn"], old);
        let observed = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "turn", tid, "--follow"],
        );
        assert!(observed.is_clean_exit(0), "{}", observed.summary());
        // `--follow` 只输出所跟的那一轮，不含正在执行的新一轮。
        let followed = records(&observed);
        assert!(followed.entries().iter().all(|entry| entry.turn_id == tid));
        assert_eq!(
            followed.closed_result().map(|(_, t, o)| (t, o)),
            Some((tid, "completed"))
        );
        assert_eq!(followed.reply_text(), "answer");
        gate.release_next(wait(&budget)).unwrap();
        let second = closed(&world, &budget, &mut evidence, winners[0]);
        assert_eq!(second["summary"]["outcome"], "completed");
        assert_eq!(detail(&world, &budget, &mut evidence, tid), old);
        assert_eq!(server.captured_requests().len(), 2);
        archive_state(&world, &budget, &mut evidence, &[tid, winners[0]]);
        evidence
            .record_json(
                "parameters",
                &json!({"script":"two gated answer Turns","old_turn":tid,
            "head_during_live":format!("{head:?}"),"followed":lines(&observed)}),
            )
            .unwrap();
        world.stop_daemon().unwrap();
        assert_eq!(detail(&world, &budget, &mut evidence, winners[0]), second);
        let tree = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(tree["nodes"].as_array().unwrap().len(), 5);
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn backend_tls_roots_are_shared_with_tools_and_fixed_until_restart() {
    use lattice_system_harness::{
        FakeLlmServer, FixtureHttpServer, HttpFixture, ProviderFixture, TlsIdentity,
    };
    let (world, spec) = direct_world(
        "background-startup-tls",
        "background",
        "[\"fetch__web\"]",
        "\n[llm.retry]\nmax_retries=0\n[lancet.capabilities]\nmax_boundary=\"ex\"\n[lancet.network]\nallow_localhost=true\nallowed_hosts=[\"localhost\"]\n",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let trusted = TlsIdentity::generate().unwrap();
        let foreign = TlsIdentity::generate().unwrap();
        let own = FixtureHttpServer::start_tls(HttpFixture::plain("trusted tool body"), &trusted)
            .unwrap();
        let cross = FixtureHttpServer::start_tls(HttpFixture::plain("foreign tool body"), &foreign)
            .unwrap();
        let root = world.root().join("startup-ca.pem");
        // 一张可用根与一张损坏证书并存时，后台仍使用可用根。
        let mut certificates = trusted.ca_pem().to_vec();
        certificates
            .extend_from_slice(b"\n-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n");
        std::fs::write(&root, certificates).unwrap();
        world.set_env("SSL_CERT_FILE", root.display().to_string());
        let (server, gate) = FakeLlmServer::start_gated_with(
            support::sequence(vec![
                SseScript::ToolCalls {
                    calls: vec![
                        (
                            "own".into(),
                            "fetch__web".into(),
                            json!({"url":own.base_url()}).to_string(),
                        ),
                        (
                            "cross".into(),
                            "fetch__web".into(),
                            json!({"url":cross.base_url()}).to_string(),
                        ),
                    ],
                },
                SseScript::plain("shared startup roots"),
                SseScript::plain("still trusted"),
            ]),
            &trusted,
        )
        .unwrap();
        world
            .wire_provider(
                ProviderFixture::chat_completions("background"),
                &server,
                FAKE_API_KEY,
            )
            .unwrap();
        let first = command(
            &world,
            &budget,
            &["--json", "run", "TLS", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        // 后台已持有 DER；文件与后续前端环境的变化都不替换这份信任。
        std::fs::write(&root, foreign.ca_pem()).unwrap();
        gate.release(wait(&budget)).unwrap();
        let out = first.wait(Some(wait(&budget))).unwrap();
        evidence.record_command("provider and Tool startup roots", &out);
        assert!(out.is_clean_exit(0), "{}", out.summary());
        let turn = detail(&world, &budget, &mut evidence, &accepted_turn(&out));
        assert_eq!(turn["steps"][0]["slots"][0]["outcome"], "succeeded");
        assert_eq!(turn["steps"][0]["slots"][1]["outcome"], "failed");
        let cross =
            &turn["steps"][0]["slots"][1]["evidence"]["value"]["effects"]["effects"][0]["network"];
        assert_eq!(cross["transport"]["tls"]["state"], "failed");
        assert_eq!(cross["failure_phase"], "tls");
        let next = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "same roots", "--agent", "background"],
        );
        assert!(next.is_clean_exit(0));
        assert_eq!(server.captured_requests().len(), 3);
        world.stop_daemon().unwrap();
        let changed = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "new roots", "--agent", "background"],
        );
        assert!(changed.is_clean_exit(1));
        let turn = closed(&world, &budget, &mut evidence, &accepted_turn(&changed));
        assert_eq!(
            turn["steps"][0]["attempts"][0]["end"]["cause"]["cause"],
            "provider_error"
        );
        assert_eq!(server.captured_requests().len(), 3);
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn concurrent_turns_keep_frozen_network_policies_with_shared_startup_roots() {
    use lattice_system_harness::{
        FakeLlmServer, FixtureHttpServer, HttpFixture, ProviderFixture, ProviderTarget, TlsIdentity,
    };
    let (world, spec) = direct_world(
        "background-network-isolation",
        "background",
        "[\"fetch__web\"]",
        "\n[lancet.capabilities]\nmax_boundary=\"ex\"\n[lancet.network]\nallow_localhost=true\nallowed_hosts=[\"localhost\"]\n",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let identity = TlsIdentity::generate().unwrap();
        let root = world.root().join("common-ca.pem");
        std::fs::write(&root, identity.ca_pem()).unwrap();
        world.set_env("SSL_CERT_FILE", root.display().to_string());
        let web =
            FixtureHttpServer::start_tls(HttpFixture::plain("allowed fetch"), &identity).unwrap();
        let script = || {
            support::sequence(vec![
                tool_call(
                    "fetch",
                    "fetch__web",
                    &json!({"url":web.base_url()}).to_string(),
                ),
                SseScript::plain("policy observed"),
            ])
        };
        let (allowed, allowed_gate) = FakeLlmServer::start_gated_with(script(), &identity).unwrap();
        let (denied, denied_gate) = FakeLlmServer::start_gated_with(script(), &identity).unwrap();
        let fixture = ProviderFixture {
            target: ProviderTarget::Custom("allowed"),
            ..ProviderFixture::chat_completions("background")
        };
        world
            .wire_provider(fixture, &allowed, FAKE_API_KEY)
            .unwrap();
        world
            .wire_provider(
                ProviderFixture {
                    target: ProviderTarget::Custom("denied"),
                    ..fixture
                },
                &denied,
                FAKE_API_KEY,
            )
            .unwrap();
        let allowed_path = world
            .project_dir()
            .join(".lattice/agents/background/agent.toml");
        let allowed_source = std::fs::read_to_string(&allowed_path)
            .unwrap()
            .replace("local/background", "allowed/background");
        let denied_source = allowed_source
            .replace("name = \"background\"", "name = \"denied\"")
            .replace("allowed/background", "denied/background")
            .replace("allow_localhost=true", "allow_localhost=false");
        std::fs::write(&allowed_path, &allowed_source).unwrap();
        world
            .write_project_file(".lattice/agents/denied/agent.toml", &denied_source)
            .unwrap();
        let first = command(
            &world,
            &budget,
            &["--json", "run", "allow", "--agent", "background"],
        );
        allowed_gate.wait_until_stalled(1, wait(&budget)).unwrap();
        let second = command(
            &world,
            &budget,
            &["--json", "run", "deny", "--agent", "denied"],
        );
        denied_gate.wait_until_stalled(1, wait(&budget)).unwrap();
        assert_eq!(world.daemon_pids().unwrap().len(), 1);
        // 两轮已接受；互换源文件中的策略不改变各自冻结的网络权限。
        std::fs::write(
            &allowed_path,
            allowed_source.replace("allow_localhost=true", "allow_localhost=false"),
        )
        .unwrap();
        world
            .write_project_file(
                ".lattice/agents/denied/agent.toml",
                &denied_source.replace("allow_localhost=false", "allow_localhost=true"),
            )
            .unwrap();
        allowed_gate.release(wait(&budget)).unwrap();
        denied_gate.release(wait(&budget)).unwrap();
        let first = first.wait(Some(wait(&budget))).unwrap();
        let second = second.wait(Some(wait(&budget))).unwrap();
        evidence.record_command("allowed Turn", &first);
        evidence.record_command("denied Turn", &second);
        assert!(first.is_clean_exit(0));
        assert!(second.is_clean_exit(0));
        let allowed_turn = closed(&world, &budget, &mut evidence, &accepted_turn(&first));
        let denied_turn = closed(&world, &budget, &mut evidence, &accepted_turn(&second));
        assert_eq!(allowed_turn["steps"][0]["slots"][0]["outcome"], "succeeded");
        let denied_slot = &denied_turn["steps"][0]["slots"][0];
        assert_eq!(denied_slot["outcome"], "failed");
        let network = &denied_slot["evidence"]["value"]["denied"]["denied"][0]["network"];
        assert_eq!(network["policy_verdict"], "denied");
        assert_eq!(network["resolution"], "not_attempted");
        assert_eq!(network["dns_candidates"], json!([]));
        assert_eq!(network["transport"]["tls"]["state"], "not_attempted");
        assert!(network["transport"].get("connected_peer").is_none());
        assert_eq!(
            web.accepted_connections_after_probe(wait(&budget)).unwrap(),
            1
        );
        assert_eq!(web.captured_requests().len(), 1);
        assert_eq!(web.rejected_handshakes(), 0);
        assert_eq!(allowed.captured_requests().len(), 2);
        assert_eq!(denied.captured_requests().len(), 2);
        evidence.record_captures(allowed.captured_requests());
        evidence.record_captures(denied.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn no_usable_startup_tls_root_rejects_all_submissions_before_acceptance() {
    let (world, spec) = direct_world("background-no-tls-root", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let root = world.root().join("invalid-ca.pem");
        std::fs::write(&root, "invalid certificate").unwrap();
        world.set_env("SSL_CERT_FILE", root.display().to_string());
        let directory = world.home().join(".lattice");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("providers.toml"), "[providers.local]\nprotocol='chat-completions'\nbase_url='http://localhost:9/v1'\nauth='none'\n").unwrap();
        let out = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "no roots", "--agent", "background"],
        );
        assert!(out.is_clean_exit(2));
        let sent = records(&out);
        let error = sent.rejection().unwrap();
        assert_eq!(error.kind, "bad_request");
        assert!(error.message.contains("no usable TLS root"));
        assert!(sent.accepted_turn_id().is_none());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn invalidated_provider_configuration_records_last_resolved_origin_without_sending() {
    for removed in [false, true] {
        let (world, spec) = direct_world(
            if removed {
                "provider-deleted"
            } else {
                "provider-invalid"
            },
            "background",
            "[\"session__todo_write\"]",
            "\n[llm.retry]\nmax_retries=3\n",
        );
        let budget = world.budget(&spec);
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (server, gate) = wire_gated_fake_llm(
                &world,
                "background",
                support::sequence(vec![
                    tool_call(
                        "todo",
                        "session__todo_write",
                        r#"{"todos":[{"content":"configured","status":"pending"}]}"#,
                    ),
                    SseScript::plain("must not receive another request"),
                ]),
            );
            let front = command(
                &world,
                &budget,
                &[
                    "--json",
                    "run",
                    "configuration changed",
                    "--agent",
                    "background",
                ],
            );
            gate.wait_until_stalled(1, wait(&budget)).unwrap();
            std::fs::write(
                world.home().join(".lattice/providers.toml"),
                if removed {
                    ""
                } else {
                    "[providers.local]\nprotocol='sk-invalid-config-diagnostic-a19bd57f'\n"
                },
            )
            .unwrap();
            gate.release(wait(&budget)).unwrap();
            let out = front.wait(Some(wait(&budget))).unwrap();
            evidence.record_command("configuration invalidated during Turn", &out);
            assert!(out.is_clean_exit(1));
            let turn = closed(&world, &budget, &mut evidence, &accepted_turn(&out));
            assert_eq!(turn["steps"].as_array().unwrap().len(), 2);
            let first = &turn["steps"][0]["attempts"][0];
            let attempts = turn["steps"][1]["attempts"].as_array().unwrap();
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0]["origin"], first["origin"]);
            assert_eq!(attempts[0]["origin"]["provider_id"], "local");
            assert_eq!(attempts[0]["origin"]["protocol"], "chat-completions");
            assert_eq!(attempts[0]["end"]["cause"]["cause"], "provider_error");
            assert_eq!(attempts[0]["end"]["cause"]["retryable"], false);
            let diagnostic = attempts[0]["end"]["diagnostic"].as_str().unwrap();
            assert!(diagnostic.contains("configuration error"));
            assert!(!diagnostic.contains("sk-invalid-config-diagnostic-a19bd57f"));
            assert!(
                turn["summary"]["notice"]["text"]
                    .as_str()
                    .unwrap()
                    .contains(diagnostic)
            );
            assert_eq!(server.captured_requests().len(), 1);
            evidence.record_captures(server.captured_requests());
        }));
        support::finish_recorded(evidence, result);
    }
}

#[test]
fn long_session_pages_by_turn_and_rewind_replaces_the_path() {
    let (world, spec) = direct_world("background-history-pages", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let server = wire_fake_llm(&world, "background", SseScript::plain("turn reply"));
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        // 12 轮单步 Turn：第 n 轮的 UserMessage 是节点 2n-1，Step 是节点 2n。
        let turns = 12_u64;
        for n in 1..=turns {
            let sent = send.send(&format!("turn {n}")).unwrap();
            evidence.record_command(format!("turn {n}"), &sent.command);
            assert_clean_send(&sent).unwrap();
            assert_accepted_and_closed(&sent).unwrap();
            // `session send --json` 只输出本轮：一条 UserMessage 与一个 Step。
            let entries = sent.entries();
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].node, 2 * n - 1);
            assert_eq!(entries[0].user_ordinal(), Some(n));
        }
        let sid = send.session_id().unwrap();
        let user = |n: u64| 2 * n - 1;
        let mut watcher = Watcher::open(&world, wait(&budget), sid);
        let view = watcher.initial();
        assert_eq!(view.head.as_ref().unwrap().position, 2 * turns);
        // 视图开始时只给最近 10 轮，轮次序号仍从根数起。
        let first_loaded = turns + 1 - WATCH_TURNS as u64;
        assert_eq!(
            view.entries.keys().copied().collect::<Vec<_>>(),
            (user(first_loaded)..=2 * turns).collect::<Vec<_>>()
        );
        assert_eq!(
            view.entries
                .values()
                .filter_map(|entry| entry.user_ordinal())
                .collect::<Vec<_>>(),
            (first_loaded..=turns).collect::<Vec<_>>()
        );
        let watched: Vec<Value> = view.raw_entries().into_iter().cloned().collect();
        // 第 8～10 轮在视图中对应的条目，供与同一区间的历史页对照。
        let watched_page: Vec<Value> = view
            .entries
            .range(user(8)..user(11))
            .map(|(_, entry)| entry.raw.clone())
            .collect();
        // 往上翻：以已装入的最早一轮的 UserMessage 为 `before`，余下不足请求的轮数。
        let earlier = history(&world, wait(&budget), sid, user(first_loaded), 10).unwrap();
        assert_eq!(
            earlier
                .iter()
                .map(|entry| entry["node"].clone())
                .collect::<Vec<_>>(),
            (1..user(first_loaded)).map(Value::from).collect::<Vec<_>>()
        );
        assert_eq!(earlier[0]["ordinal"], 1);
        assert_eq!(earlier[2]["ordinal"], 2);
        // 页的边界只落在 UserMessage 之前：三轮正好是六个节点，首条是 UserMessage。
        let page = history(&world, wait(&budget), sid, user(11), 3).unwrap();
        assert_eq!(
            page.iter()
                .map(|entry| entry["node"].clone())
                .collect::<Vec<_>>(),
            (user(8)..user(11)).map(Value::from).collect::<Vec<_>>()
        );
        assert_eq!(page[0]["content"]["kind"], "user_message");
        assert_eq!(page[0]["ordinal"], 8);
        // 历史查询与视图给出同一种条目：同一区间逐字段相同。
        assert_eq!(page, watched_page);
        let zero = history(&world, wait(&budget), sid, user(11), 0).unwrap_err();
        assert_eq!(zero["response"], "error", "{zero}");
        assert_eq!(zero["value"]["kind"], "bad_request", "{zero}");
        // 当前位置改变后，同一历史查询结果不变；已打开的观察者收到路径已替换。
        let rewound = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "rewind", sid, "12"],
        );
        assert_eq!(rewound["current"], 12);
        assert_eq!(
            history(&world, wait(&budget), sid, user(11), 3).unwrap(),
            page
        );
        let replaced = watcher.until(
            Instant::now() + wait(&budget),
            "the replaced path up to node 12",
            |view| {
                view.head
                    .as_ref()
                    .is_some_and(|head| head.path_replaced && head.position == 12)
                    && view.entries.contains_key(&12)
            },
        );
        assert_eq!(
            replaced.entries.keys().copied().collect::<Vec<_>>(),
            (1..=12).collect::<Vec<_>>()
        );
        let reopened = Watcher::open(&world, wait(&budget), sid)
            .initial()
            .raw_entries()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            reopened,
            replaced
                .raw_entries()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        );
        assert_eq!(server.captured_requests().len(), turns as usize);
        evidence
            .record_json(
                "pages",
                &json!({"watched":watched,"earlier":earlier,"page":page,"after_rewind":watcher.received}),
            )
            .unwrap();
        support::record_session(
            &send,
            &mut evidence,
            sid,
            json!({"turns":turns,"watch_turns":WATCH_TURNS,"rewind":12}),
        );
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn vanished_owner_is_reported_by_the_session_head() {
    let (world, spec) = direct_world("background-vanished-owner", "background", "[]", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (server, gate) = wire_gated_fake_llm(&world, "background", SseScript::plain("next"));
        world.stop_daemon().unwrap();
        let root = world
            .session_store_dir()
            .parent()
            .unwrap()
            .display()
            .to_string();
        let daemon = command(&world, &budget, &["daemon", "serve", "--state-root", &root]);
        let deadline = Instant::now() + wait(&budget);
        while !run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "daemon", "status"],
        )
        .is_clean_exit(0)
        {
            assert!(Instant::now() < deadline, "explicit daemon did not start");
            std::thread::yield_now();
        }
        assert_eq!(world.daemon_pids().unwrap(), vec![daemon.pgid()]);
        let front = command(
            &world,
            &budget,
            &["--json", "run", "vanish", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        let (sid, tid) = running_turns(&world, &budget, &mut evidence)[0].clone();
        let killed = daemon.kill_and_wait().unwrap();
        evidence.record_command("SIGKILL backend during the request", &killed);
        assert_forced_kill(&killed);
        let lost = front.wait(None).unwrap();
        evidence.record_command("frontend loses the backend", &lost);
        assert!(lost.is_clean_exit(1), "{}", lost.summary());
        let stopped = detail(&world, &budget, &mut evidence, &tid);
        assert_ne!(stopped["summary"]["progress"]["progress"], "closed");
        // 重新打开会话：会话头直接说明当前位置那一轮没有 Owner，不必先观察那一轮。
        let mut watcher = Watcher::open(&world, wait(&budget), &sid);
        let head = watcher.initial().head.clone().unwrap();
        assert_eq!(head.turn_id.as_deref(), Some(tid.as_str()));
        assert!(!head.owner, "{head:?}");
        let followed = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "turn", &tid, "--follow"],
        );
        assert!(followed.is_clean_exit(1), "{}", followed.summary());
        let followed = records(&followed);
        assert_eq!(
            followed.closed_result(),
            Some((sid.as_str(), tid.as_str(), "failed"))
        );
        let last = followed.entries().into_iter().last().unwrap();
        assert_eq!(last.step().unwrap().progress, StepProgress::Requesting);
        // 用户直接做下一件事：新一轮被接受并执行，旧 Turn 的节点保持原样。
        gate.release(wait(&budget)).unwrap();
        let next = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "after vanish", "--session", &sid],
        );
        assert!(next.is_clean_exit(0), "{}", next.summary());
        assert_eq!(detail(&world, &budget, &mut evidence, &tid), stopped);
        assert_eq!(server.captured_requests().len(), 2);
        archive_state(&world, &budget, &mut evidence, &[&tid]);
        evidence
            .record_json(
                "parameters",
                &json!({"turn":tid,"session":sid,"head":format!("{head:?}"),"watch":watcher.received}),
            )
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn turn_and_session_identity_conflicts_are_rejected_without_dispatch() {
    let (world, spec) = direct_world(
        "background-global-turn-identity",
        "background",
        "[\"session__todo_write\"]",
        "",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let server = wire_fake_llm(
            &world,
            "background",
            SseScript::Cycle {
                scripts: vec![
                    tool_call(
                        "todo",
                        "session__todo_write",
                        r#"{"todos":[{"content":"shared","status":"pending"}]}"#,
                    ),
                    SseScript::plain("answer"),
                ],
            },
        );
        let first = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "alpha", "--agent", "background"],
        );
        let second = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "beta", "--agent", "background"],
        );
        assert!(first.is_clean_exit(0) && second.is_clean_exit(0));
        let (first_turn, second_turn) = (accepted_turn(&first), accepted_turn(&second));
        let a = detail(&world, &budget, &mut evidence, &first_turn);
        let b = detail(&world, &budget, &mut evidence, &second_turn);
        assert_ne!(a["session_id"], b["session_id"]);
        let config = configuration(&world, "background", &server.base_url());
        let submit = |turn: &str, session: Value| {
            ipc(
                &world,
                wait(&budget),
                submit_request(&world, turn, session, "alpha", None, config.clone()),
            )
        };
        // 请求身份表自上而下核对，先符合的一行决定结果：TurnId 先于会话。
        let used_elsewhere = submit(&first_turn, json!({"existing": b["session_id"]}));
        assert_eq!(
            conflict(&used_elsewhere),
            Some("turn_id_used"),
            "{used_elsewhere}"
        );
        let used_new = submit(&first_turn, json!({"new": a["session_id"]}));
        assert_eq!(conflict(&used_new), Some("turn_id_used"), "{used_new}");
        let exists = submit(THIRD, json!({"new": a["session_id"]}));
        assert_eq!(conflict(&exists), Some("session_exists"), "{exists}");
        let missing = submit(THIRD, json!({"existing": NEW_SESSION}));
        assert_eq!(missing["value"]["kind"], "not_found", "{missing}");
        let project = world.root().join("different-project");
        std::fs::create_dir(&project).unwrap();
        gix::init(&project).unwrap();
        let mut elsewhere = submit_request(
            &world,
            THIRD,
            json!({"existing": a["session_id"]}),
            "alpha",
            None,
            config.clone(),
        );
        elsewhere["request"]["workspace"] = json!(project);
        let binding = ipc(&world, wait(&budget), elsewhere);
        assert_eq!(conflict(&binding), Some("binding_mismatch"), "{binding}");
        // 被拒绝的请求都没有建立 Turn：THIRD 仍查不到，会话数不变。
        assert_eq!(
            turn_lookup(&world, wait(&budget), THIRD)["value"]["kind"],
            "not_found"
        );
        assert_eq!(
            read(
                &world,
                &budget,
                &mut evidence,
                &["--json", "session", "list"]
            )
            .as_array()
            .unwrap()
            .len(),
            2
        );
        evidence
            .record_json(
                "identity-conflicts",
                &json!({"turn_used_elsewhere":used_elsewhere,"turn_used_new":used_new,
                    "session_exists":exists,"missing":missing,"binding":binding}),
            )
            .unwrap();
        archive_state(&world, &budget, &mut evidence, &[&first_turn, &second_turn]);
        evidence.record_json("parameters", &json!({"turns":[first_turn,second_turn],"projects":2,"script":"two todo/final Turns; rejected identities"})).unwrap();
        assert_eq!(detail(&world, &budget, &mut evidence, &first_turn), a);
        assert_eq!(detail(&world, &budget, &mut evidence, &second_turn), b);
        assert_eq!(server.captured_requests().len(), 4);
        let inv_a = &a["steps"][0]["slots"][0]["invocation_id"];
        let inv_b = &b["steps"][0]["slots"][0]["invocation_id"];
        assert_ne!(inv_a, inv_b);
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}
