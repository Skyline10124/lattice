// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 前后端分离、回执丢失与按轮次标识找回、身份冲突、忙拒绝与并发、视图推送与历史查询、
//! 取消与 `daemon stop` 的真实二进制场景。提交与视图边界经公开本地 IPC，会话事实经公开 CLI 读取。
mod support;
use lattice_system_harness::{
    CancelRecord, CancelStatus, CommandOutcome, DaemonOptions, FAKE_API_KEY, LatticeCommand,
    RunningCommand, ScenarioBudget, ScenarioEvidence, ScenarioWorld, SendOutcome, SendRecord,
    SessionSendDriver, SseScript, StepProgress, merge_segments,
};
use serde_json::{Value, json};
#[cfg(feature = "fake-tools")]
use std::path::Path;
use std::time::{Duration, Instant};
#[cfg(feature = "fake-tools")]
use support::tool_call;
use support::{
    Watcher, Wire, assert_accepted_and_closed, assert_clean_send, assert_forced_kill,
    configure_request, direct_world, driver, history, ipc, parse_send_stdout, submit_request,
    turn_closed, turn_lookup, wire_fake_llm, wire_gated_fake_llm,
};
const FIRST: &str = "00000000-0000-4000-8000-000000000001";
const SECOND: &str = "00000000-0000-4000-8000-000000000002";
const THIRD: &str = "00000000-0000-4000-8000-000000000003";
/// 客户端为新会话生成的会话标识（32 位小写十六进制）。
const NEW_SESSION: &str = "5e550000000000000000000000000001";
/// `watch` 开始时给出的轮数。
const WATCH_TURNS: usize = 10;
#[cfg(feature = "fake-tools")]
const TEST_TOOLS: &str = "LATTICE_TEST_TOOLS";
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
    evidence.record_command(format!("执行命令（{}）", args.join(" ")), &out);
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
/// CLI 生成并经接受回执确认的轮次标识。
fn accepted_turn(out: &CommandOutcome) -> String {
    records(out)
        .accepted_turn_id()
        .expect("应有已接受的轮次标识")
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
        assert!(Instant::now() < deadline, "未在截止时间前等到轮次收束");
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
#[cfg(feature = "fake-tools")]
fn marker_lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => content.lines().map(str::to_owned).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("读取假工具标记文件 {} 失败：{error}", path.display()),
    }
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
        records.push(record);
    }
    evidence
        .record_json("state-summary", &json!({"list":list,"turns":records}))
        .unwrap();
}
fn finish(evidence: ScenarioEvidence, result: std::thread::Result<()>) {
    let primary = result.err().map(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "非文本 panic 载荷".into())
    });
    evidence.finish_report(primary).unwrap();
}
#[test]
fn cli_killed_after_accept_completes_turn_for_later_follow() {
    let (world, spec) = direct_world("background-cli-detach", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
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
        // 另起的客户端按轮次标识查询：已接受，属于同一会话。
        let found = turn_lookup(&world, wait(&budget), &tid);
        assert_eq!(found["response"], "turn", "{found}");
        assert_eq!(found["value"]["status"], "accepted");
        assert_eq!(found["value"]["turn"]["session_id"], sid);
        let owners = world.daemon_pids().unwrap();
        assert_eq!(owners.len(), 1);
        assert_ne!(owners[0], front.pgid());
        let killed = front.kill_and_wait().unwrap();
        evidence.record_command("接受回执后强制分离前端 CLI", &killed);
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
        evidence
            .record_json(
                "parameters",
                &json!({
                    "turn": tid,
                    "script": "gated plain detached durable answer",
                    "frontend": "SIGKILL after acceptance",
                    "backend_pids": owners,
                }),
            )
            .unwrap();
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
                "",
            );
            let budget = world.budget(&spec);
            let mut evidence = support::evidence(&world, &spec);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                world.start_daemon().expect("显式启动后台");
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
                // 先用只读的 `status` 确认后台说同一协议，再进入未读回执窗口；
                // 协议不符时在这里直接失败，而不是表现为 provider 一直等不到请求。
                let status = ipc(&world, wait(&budget), json!({"operation":"status"}));
                assert_eq!(status["response"], "status", "{status}");
                if !existing_session {
                    let configured = ipc(
                        &world,
                        wait(&budget),
                        configure_request(
                            &world,
                            json!({"new": &sid}),
                            Some("background"),
                            None,
                            None,
                        ),
                    );
                    assert_eq!(configured["response"], "configure", "{configured}");
                }
                let request = submit_request(&world, FIRST, &sid, "identical input");
                let unread = Wire::connect(&world, wait(&budget), &request);
                gate.wait_until_stalled(1, wait(&budget)).unwrap();
                // 此连接只写请求，从未读取回执；按 `TurnId` 查询得到已接受。
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
                    // `owner` 仍在执行时以同一身份重新提交：`TurnId` 已被使用，不再兼做找回。
                    let retry = ipc(&world, wait(&budget), request.clone());
                    assert_eq!(conflict(&retry), Some("turn_id_used"), "{retry}");
                    evidence.record_json("running-retry", &retry).unwrap();
                }
                gate.release(wait(&budget)).unwrap();
                let original = closed(&world, &budget, &mut evidence, FIRST);
                if restart {
                    world.restart_daemon().expect("显式重启后台");
                }
                // 后台持续运行或重启后，同一 `TurnId` 都查到同一个已关闭的轮次。
                assert_eq!(detail(&world, &budget, &mut evidence, FIRST), original);
                let reread = turn_lookup(&world, wait(&budget), FIRST);
                assert_eq!(reread["value"]["status"], "accepted", "{reread}");
                assert_eq!(reread["value"]["turn"], original);
                let current_epoch =
                    ipc(&world, wait(&budget), json!({"operation":"status"}))["value"]["epoch"]
                        .clone();
                assert_eq!(epoch != current_epoch, restart);
                // 新建会话时 `TurnId` 先于会话核对，得到的是 `TurnId` 冲突而不是会话已存在。
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
    let (world, spec) = direct_world("background-concurrent-stop", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
        let initial_pgid = world.daemon_pgid().expect("自有后台进程组");
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
            .expect("第二个会话应存在正在运行的轮次");
        let busy = ipc(
            &world,
            wait(&budget),
            submit_request(&world, THIRD, &a, "must not queue"),
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
        // 先结束提交这一轮的前端：门控未放行，这一轮仍在执行，强制结束的是仍在观察的进程。
        // 放在取消之后会与后台关闭这一轮竞争：前端可能先看到取消结果而自行以退出码 2 退出。
        let detached = first.kill_and_wait().unwrap();
        evidence.record_command("取消前强制分离提交端 CLI", &detached);
        assert_forced_kill(&detached);
        // 另一个终端按会话取消：前端从会话头查出正在执行的那一轮再按 `TurnId` 取消。
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
        assert_eq!(
            closed(&world, &budget, &mut evidence, &first_turn)["summary"]["outcome"],
            "cancelled"
        );
        // 持久关闭先于监督者移除 `owner`；无可取消的断言必须等到后台完成收尾。
        let deadline = Instant::now() + wait(&budget);
        let mut watcher = Watcher::open(&world, wait(&budget), &a);
        watcher.until(deadline, "已取消轮次的占有方释放完毕", |view| {
            view.head.as_ref().is_some_and(|head| {
                !head.owner && head.turn_id.as_deref() == Some(first_turn.as_str())
            })
        });
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
                submit_request(&world, THIRD, &a, "after stop")
            )["value"]["kind"],
            "stopping"
        );
        assert_ne!(
            detail(&world, &budget, &mut evidence, &second_turn)["summary"]["progress"]["progress"],
            "closed"
        );
        // `stop` 必须等待被门控挡住的在途轮次，不能在释放前就返回。
        assert!(
            !stop.has_exited().unwrap(),
            "在途工作尚未收尾时 `daemon stop` 不得提前返回"
        );
        gate.release(wait(&budget)).unwrap();
        let done = second.wait(None).unwrap();
        evidence.record_command("并行会话正常完成", &done);
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
        evidence.record_command("后台排空在途轮次后停止", &drained);
        assert!(drained.is_clean_exit(0));
        assert_eq!(lines(&drained)[0]["owners"], 0);
        world.daemon_departed().unwrap();
        assert!(
            lattice_system_harness::processes_in_pgid(initial_pgid)
                .unwrap()
                .is_empty(),
            "后台排空退出后原进程组内不应残留任何进程"
        );
        world.stop_daemon().expect("排空停止后不应存在残留后台");
        world
            .start_daemon()
            .expect("排空退出后显式启动后台以核对持久结果");
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
fn concurrent_lattice_daemon_has_one_owner_and_midway_watch_matches_history() {
    struct CandidateGuard(Vec<RunningCommand>);
    impl Drop for CandidateGuard {
        fn drop(&mut self) {
            for candidate in self.0.drain(..) {
                let _ = candidate.kill_and_wait();
            }
        }
    }

    let (world, spec) = direct_world("background-start-watch", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let large = "大".repeat(24000);
        world
            .start_daemon()
            .expect("先显式启动后台以录入 provider 凭据");
        let (server, gate) = wire_gated_fake_llm(
            &world,
            "background",
            support::sequence(vec![
                SseScript::plain(large.clone()),
                SseScript::plain(large.clone()),
                SseScript::plain("after"),
            ]),
        );
        world.stop_daemon().expect("并发启动前先停止已有后台");
        assert!(
            world.daemon_pids().unwrap().is_empty(),
            "并发启动前不应存在运行中的自有后台"
        );
        let root = world
            .session_store_dir()
            .parent()
            .unwrap()
            .display()
            .to_string();
        let daemon_drv = support::daemon_driver();
        const CONCURRENCY: usize = 4;
        let barrier = std::sync::Barrier::new(CONCURRENCY);
        let spawned = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..CONCURRENCY)
                .map(|_| {
                    let world = &world;
                    let budget = &budget;
                    let barrier = &barrier;
                    let root = &root;
                    let daemon_drv = &daemon_drv;
                    scope.spawn(move || {
                        barrier.wait();
                        daemon_drv
                            .start(
                                world,
                                &LatticeCommand {
                                    args: vec!["--state-root".into(), root.clone()],
                                    timeout: Some(wait(budget)),
                                },
                            )
                            .unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut guard = CandidateGuard(spawned);
        let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(5));
        loop {
            let mut alive = 0;
            for proc in &mut guard.0 {
                if !proc.has_exited().unwrap() {
                    alive += 1;
                }
            }
            if alive == 1 {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    let probe = driver()
                        .start(
                            &world,
                            &LatticeCommand {
                                args: vec!["--json".into(), "daemon".into(), "status".into()],
                                timeout: Some(remaining),
                            },
                        )
                        .unwrap()
                        .wait(None)
                        .unwrap();
                    evidence.record_command("轮询后台状态（--json daemon status）", &probe);
                    if probe.is_clean_exit(0)
                        && serde_json::from_slice::<Value>(&probe.stdout)
                            .is_ok_and(|status| status["stopping"] == Value::Bool(false))
                    {
                        break;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "并发 `lattice-daemon` 未在就绪截止时间内收敛为唯一就绪持有者，当前存活数：{alive}"
            );
            std::thread::sleep(
                Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
            );
        }

        let mut winners = Vec::new();
        let mut loser_outs = Vec::new();
        for mut candidate in guard.0.drain(..) {
            if candidate.has_exited().unwrap() {
                let out = candidate.wait(Some(wait(&budget))).unwrap();
                evidence.record_command("并发争抢失败退出的 lattice-daemon", &out);
                loser_outs.push(out);
            } else {
                winners.push(candidate);
            }
        }
        assert_eq!(winners.len(), 1, "同一状态根上只能有一个持有者存活");
        let winner_pid = winners[0].pgid();
        let expected_diag = format!("状态根已被另一个后台占用（对端进程号：{winner_pid}）");
        let mut loser_codes = Vec::new();
        for out in loser_outs {
            assert!(
                out.is_clean_exit(3),
                "争抢状态根失败的 `lattice-daemon` 必须干净地以退出码 3 退出：{}",
                out.summary()
            );
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains(&expected_diag),
                "争抢失败的标准错误应附带对端进程号 `{expected_diag}`，实际：{stderr}"
            );
            loser_codes.push(out.exit_code().unwrap());
        }
        assert_eq!(loser_codes, vec![3; CONCURRENCY - 1]);
        guard.0 = winners;

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
        let running = running_turns(&world, &budget, &mut evidence);
        assert_eq!(running.len(), 2);
        let (sid, tid) = running[0].clone();
        // 中途开始的观察：会话头说明当前位置那一轮有 `owner`，随后是这一轮的条目。
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
            evidence.record_command("并发胜出后台处理首轮发送", &done);
            assert!(done.is_clean_exit(0), "{}", done.summary());
            let sent = records(&done);
            assert_accepted_and_closed(&sent).unwrap();
            assert_eq!(sent.reply_text(), large);
        }
        watcher.until(
            Instant::now() + wait(&budget),
            "中途接入观察的轮次收束",
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
            "loser_exit_codes":loser_codes,"midway":midway,"held":held,"history":reread}),
            )
            .unwrap();
        evidence.record_captures(server.captured_requests());

        let stopped = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "daemon", "stop"],
        );
        assert!(stopped.is_clean_exit(0), "{}", stopped.summary());
        let winner = guard.0.pop().expect("存活的唯一持有者");
        let winner_out = winner.wait(Some(wait(&budget))).unwrap();
        evidence.record_command("并发胜出的 lattice-daemon 正常退出", &winner_out);
        assert!(
            winner_out.is_clean_exit(0),
            "唯一持有者正常停机应以退出码 0 退出：{}",
            winner_out.summary()
        );
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn slow_watcher_cannot_block_execution_and_converges_to_history() {
    let (world, spec) = direct_world("background-slow-watcher", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
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
        evidence.record_command("慢观察者在场时正常客户端完成发送", &done);
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
            "慢观察者追上并持有已收束轮次",
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
#[cfg(feature = "fake-tools")]
#[test]
fn shared_daemon_uses_registered_providers_and_freezes_profiles() {
    use lattice_system_harness::{FakeLlmServer, assert_no_secret};
    let (world, spec) = direct_world("background-project-config", "background", "");
    world.set_env(TEST_TOOLS, "record_note");
    let budget = world.budget(&spec);
    let key_a = "project-alpha-private-fixture-key";
    let key_b = "project-beta-private-fixture-key";
    let mut evidence =
        ScenarioEvidence::new(&world, &spec.name, [key_a.into(), key_b.into()]).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动带测试工具环境的后台");
        let script = SseScript::Sequence {
            scripts: vec![
                tool_call("note", "record_note", r#"{"action":"read"}"#),
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
        for (name, label, out) in [
            ("alpha", "alpha 项目运行发送", &first),
            ("beta", "beta 项目运行发送", &second),
        ] {
            evidence.record_command(label, out);
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
        let alpha_detail = detail(&world, &budget, &mut evidence, &alpha_turn);
        assert_eq!(alpha_detail["steps"][0]["slots"][0]["outcome"], "succeeded");
        archive_state(&world, &budget, &mut evidence, &[&alpha_turn]);
        let beta = detail(&world, &budget, &mut evidence, &beta_turn);
        assert_eq!(beta["steps"][0]["slots"][0]["outcome"], "succeeded");
        let sid = beta["session_id"].as_str().unwrap();
        for args in [
            vec!["--json", "session", "log", sid],
            vec!["session", "log", sid],
            vec!["--json", "session", "turn", &beta_turn],
        ] {
            let mut cmd = world.command(driver().binary_path());
            cmd.current_dir(&project).args(&args);
            let out = RunningCommand::spawn(cmd)
                .unwrap()
                .wait(Some(budget.cap_timeout(Duration::from_secs(30))))
                .unwrap();
            evidence.record_command(format!("beta 项目执行命令（{}）", args.join(" ")), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
        }
        evidence.record_json("beta-state", &beta).unwrap();
        evidence.record_json("parameters", &json!({"turns":[alpha_turn,beta_turn],"prompts":["alpha","beta"],"methods":["record_note"],"final_reply":"frozen result","provider_requests":[2,2],"source_changed_after_both_gates":true})).unwrap();
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn later_session_driver_keeps_the_scenario_budget_origin() {
    let (world, spec) = direct_world("background-budget-origin", "background", "");
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
    let (world, mut spec) = direct_world("background-budget-exhausted", "background", "");
    world
        .start_daemon()
        .expect("预算耗尽前显式启动自有直接子进程后台");
    let pgid = world.daemon_pgid().expect("自有后台进程组");
    assert_eq!(world.daemon_pids().unwrap(), vec![pgid]);
    spec.total_timeout_secs = Some(0);
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(budget.remaining(), Some(Duration::ZERO));
        let start = Instant::now();
        let errors = world
            .stop_daemon()
            .expect_err("场景预算已耗尽时正常停止必须记录失败");
        assert!(
            errors.iter().any(|s| s.contains("场景预算")),
            "错误信息应包含场景预算耗尽说明：{errors:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(world.daemon_pids().unwrap().is_empty());
        assert!(
            lattice_system_harness::processes_in_pgid(pgid)
                .unwrap()
                .is_empty(),
            "强制清理后自有后台进程组不得残留任何进程"
        );
        let killed = world
            .take_daemon_outcome()
            .expect("回收强制终止的直接子进程退出结果");
        evidence.record_command("场景预算耗尽后的后台强制清理", &killed);
        assert!(killed.cleanup.surviving_pids.is_empty());
        world
            .stop_daemon()
            .expect("强制清理后再次检查状态根不得存在残留后台");
    }));
    finish(evidence, result);
}
#[cfg(feature = "fake-tools")]
#[test]
fn daemon_restart_reads_closed_result_without_resubmitting() {
    let (world, spec) = direct_world("background-restart-read", "background", "");
    world.set_env(TEST_TOOLS, "write_marker");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动带测试工具环境的后台");
        let marker_path = world.root().join("restart-tool.marker");
        let server = wire_fake_llm(
            &world,
            "background",
            SseScript::Sequence {
                scripts: vec![
                    tool_call(
                        "marker",
                        "write_marker",
                        &json!({
                            "action": "append_marker",
                            "path": marker_path,
                            "line": "durable\n",
                        })
                        .to_string(),
                    ),
                    SseScript::plain("restart answer"),
                    SseScript::plain("answer 中"),
                ],
            },
        );
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        let sent = send.send("restart prompt").unwrap();
        evidence.record_command("后台重启前发送多步轮次", &sent.command);
        assert_clean_send(&sent).unwrap();
        let tid = sent.accepted_turn_id().unwrap();
        let tool_session = send.session_id().unwrap();
        let original = detail(&world, &budget, &mut evidence, tid);
        assert_eq!(original["steps"][0]["slots"][0]["outcome"], "succeeded");
        assert_eq!(marker_lines(&marker_path), vec!["durable"]);
        // 同项目另建无工具会话，保留空槽位列表和单步骤条目的独立信号。
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
        assert_eq!(plain_before["steps"][0]["slots"], json!([]));
        let sid = plain_before["session_id"].as_str().unwrap();
        world.restart_daemon().expect("显式重启后台");
        world
            .register_key(
                lattice_system_harness::ProviderFixture::chat_completions("background"),
                None,
            )
            .unwrap();
        assert_eq!(detail(&world, &budget, &mut evidence, tid), original);
        // 重启后重新打开会话：会话头说明没有 `owner`，条目给出这一轮的全部节点。
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
        // 历史查询只取 `before` 之前的轮：第 1 轮之前没有条目，`before` 不是用户消息节点时拒绝。
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
            "收到回退至根节点的路径替换会话头",
            |view| {
                view.head
                    .as_ref()
                    .is_some_and(|head| head.path_replaced && head.position == 0)
            },
        );
        assert!(replaced.entries.is_empty());
        let plain_after = detail(&world, &budget, &mut evidence, &plain_turn);
        assert_eq!(plain_after, plain_before);
        assert_eq!(plain_after["steps"][0]["slots"], json!([]));
        let log = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "log", sid],
        );
        assert_eq!(log["nodes"].as_array().unwrap().len(), 3);
        let markers = marker_lines(&marker_path);
        assert_eq!(markers, vec!["durable"]);
        evidence
            .record_json(
                "readonly-after-restart",
                &json!({"watch":plain_observer.received,"rewind":rewound,"log":log,"markers":markers}),
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
                &json!({"script":"write_marker durable then restart answer","turn":tid,"restart":true}),
            )
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}
#[test]
fn a_live_new_turn_does_not_block_old_closed_observation() {
    let (world, spec) = direct_world("background-read-beside-live", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
        let (server, gate) = wire_gated_fake_llm(&world, "background", SseScript::plain("answer"));
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        let first = send.spawn("first").unwrap();
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        gate.release_next(wait(&budget)).unwrap();
        let first = send.wait_spawned(first).unwrap();
        evidence.record_command("重启前首轮阻塞发送", &first.command);
        assert_clean_send(&first).unwrap();
        assert!(first.has_completed());
        let tid = first.accepted_turn_id().unwrap();
        let old = detail(&world, &budget, &mut evidence, tid);
        // 重启清除旧轮次的 `owner`，随后四个新身份共同争抢空闲会话。
        world.restart_daemon().expect("显式重启后台");
        detail(&world, &budget, &mut evidence, tid);
        let sid = old["session_id"].as_str().unwrap();
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
                    let request = submit_request(&world, candidate, sid, "second");
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
            "target":0,"workspace":support::path_bytes(world.project_dir())}),
        );
        assert_eq!(blocked["value"]["kind"], "busy", "{blocked}");
        // 已用的 `TurnId` 先于 `busy` 核对：同一身份重新提交得到冲突，不是找回。
        let reused = ipc(
            &world,
            wait(&budget),
            submit_request(&world, tid, sid, "first"),
        );
        assert_eq!(conflict(&reused), Some("turn_id_used"), "{reused}");
        evidence
            .record_json(
                "used-identity-beside-live",
                &json!({"rewind":blocked,"resubmit":reused}),
            )
            .unwrap();
        // 会话头直接说明当前位置那一轮（新一轮）有 `owner`；旧一轮按 `TurnId` 查到已关闭。
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
        world.restart_daemon().expect("显式重启后台");
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
fn backend_tls_roots_are_fixed_until_restart() {
    use lattice_system_harness::{FakeLlmServer, ProviderFixture, ProviderTarget, TlsIdentity};
    let (world, spec) = direct_world(
        "background-startup-tls",
        "background",
        "\n[llm.retry]\nmax_retries=0\n",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let trusted = TlsIdentity::generate().unwrap();
        let foreign = TlsIdentity::generate().unwrap();
        let root = world.root().join("startup-ca.pem");
        let foreign_root = world.root().join("foreign-ca.pem");
        // 一张可用根与一张损坏证书并存时，后台仍使用可用根。
        let mut certificates = trusted.ca_pem().to_vec();
        certificates
            .extend_from_slice(b"\n-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n");
        std::fs::write(&root, certificates).unwrap();
        std::fs::write(&foreign_root, foreign.ca_pem()).unwrap();
        world.set_env("SSL_CERT_FILE", root.display().to_string());
        world
            .start_daemon()
            .expect("显式启动带初始根证书环境的后台");
        let (server, gate) = FakeLlmServer::start_gated_with(
            support::sequence(vec![
                SseScript::plain("trusted startup roots"),
                SseScript::plain("still trusted"),
            ]),
            &trusted,
        )
        .unwrap();
        let (foreign_server, foreign_gate) =
            FakeLlmServer::start_gated_with(SseScript::plain("trusted after restart"), &foreign)
                .unwrap();
        foreign_gate.release(wait(&budget)).unwrap();
        world
            .wire_provider(
                ProviderFixture::chat_completions("background"),
                &server,
                FAKE_API_KEY,
            )
            .unwrap();
        world
            .wire_provider(
                ProviderFixture {
                    target: ProviderTarget::Custom("foreign"),
                    ..ProviderFixture::chat_completions("background")
                },
                &foreign_server,
                FAKE_API_KEY,
            )
            .unwrap();
        let first = command(
            &world,
            &budget,
            &["--json", "run", "TLS", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        // 后台启动时已取得环境快照并持有证书数据；原文件改写与后续前端环境变量切换都不替换运行中后台的信任根。
        std::fs::write(&root, foreign.ca_pem()).unwrap();
        world.set_env("SSL_CERT_FILE", foreign_root.display().to_string());
        gate.release(wait(&budget)).unwrap();
        let out = first.wait(Some(wait(&budget))).unwrap();
        evidence.record_command("校验后台启动时固定的信任根证书", &out);
        assert!(out.is_clean_exit(0), "{}", out.summary());
        assert_eq!(records(&out).reply_text(), "trusted startup roots");
        let untrusted = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "foreign before restart",
                "--agent",
                "background",
                "--model",
                "foreign/background",
            ],
        );
        assert!(untrusted.is_clean_exit(1), "{}", untrusted.summary());
        let untrusted_turn = closed(&world, &budget, &mut evidence, &accepted_turn(&untrusted));
        assert_eq!(
            untrusted_turn["steps"][0]["attempts"][0]["end"]["cause"]["cause"],
            "provider_error"
        );
        assert!(foreign_server.captured_requests().is_empty());
        assert!(foreign_server.rejected_handshakes() >= 1);
        let next = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "same roots", "--agent", "background"],
        );
        assert!(next.is_clean_exit(0), "{}", next.summary());
        assert_eq!(records(&next).reply_text(), "still trusted");
        assert_eq!(server.captured_requests().len(), 2);
        world.restart_daemon().expect("显式用新根证书环境重启后台");
        let changed = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "new roots", "--agent", "background"],
        );
        assert!(changed.is_clean_exit(1), "{}", changed.summary());
        let turn = closed(&world, &budget, &mut evidence, &accepted_turn(&changed));
        assert_eq!(
            turn["steps"][0]["attempts"][0]["end"]["cause"]["cause"],
            "provider_error"
        );
        assert_eq!(server.captured_requests().len(), 2);
        assert!(server.rejected_handshakes() >= 1);
        let reloaded = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "foreign after restart",
                "--agent",
                "background",
                "--model",
                "foreign/background",
            ],
        );
        assert!(reloaded.is_clean_exit(0), "{}", reloaded.summary());
        assert_eq!(records(&reloaded).reply_text(), "trusted after restart");
        assert_eq!(foreign_server.captured_requests().len(), 1);
        evidence.record_captures(server.captured_requests());
        evidence.record_captures(foreign_server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

/// 把任意字节编成证书块；只用于构造超过 65536 字节的“证书”，大小检查先于解析。
fn pem_certificate(der: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in der.chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let group = u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(TABLE[(group >> (18 - 6 * index) & 0x3f) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}

/// 启动时的根证书全都超过上限：后台照常启动，`https` provider 的 `session send`
/// 在接受前按配置错误拒绝且不发请求；同一后台换成 `http` provider 照常接受并完成。
#[test]
fn oversized_startup_roots_reject_https_provider_at_submit_and_serve_http() {
    use lattice_system_harness::{FakeLlmServer, ProviderFixture};
    let (world, spec) = direct_world(
        "background-oversized-roots",
        "background",
        "\n[llm.retry]\nmax_retries=0\n",
    );
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let root = world.root().join("oversized-roots.pem");
        let oversized = pem_certificate(&[0x30; 65_537]);
        std::fs::write(&root, format!("{oversized}{oversized}")).unwrap();
        world.set_env("SSL_CERT_FILE", root.display().to_string());
        world
            .start_daemon()
            .expect("显式启动带超大根证书文件的后台");

        let https = FakeLlmServer::start(SseScript::plain("must not be requested")).unwrap();
        world
            .wire_provider(
                ProviderFixture::chat_completions("background"),
                &https,
                FAKE_API_KEY,
            )
            .unwrap();
        let rejected = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "session",
                "send",
                "https",
                "--agent",
                "background",
            ],
        );
        assert!(
            rejected.is_clean_exit(2),
            "无可用根证书时 `https` provider 必须在提交阶段被拒绝：{}",
            rejected.summary()
        );
        let outcome = records(&rejected);
        assert_eq!(
            support::rejected_kind(&outcome),
            Some("bad_request"),
            "拒绝结果必须携带 `bad_request` 类别"
        );
        assert!(
            outcome.accepted_turn_id().is_none(),
            "提交被拒绝时不得接受轮次"
        );
        let stdout = String::from_utf8_lossy(&rejected.stdout);
        assert!(
            stdout.contains("configuration error")
                && stdout.contains("no usable TLS root certificates"),
            "拒绝信息必须说明缺少可用根证书：{stdout}"
        );
        assert!(
            https.captured_requests().is_empty(),
            "`https` provider 不得收到请求"
        );

        let http = FakeLlmServer::start_plain_http1(SseScript::plain("plain http answer")).unwrap();
        world
            .wire_provider(
                ProviderFixture::chat_completions("background"),
                &http,
                FAKE_API_KEY,
            )
            .unwrap();
        let served = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "send", "http", "--agent", "background"],
        );
        assert!(
            served.is_clean_exit(0),
            "无可用根证书时 `http` provider 仍应正常服务：{}",
            served.summary()
        );
        assert!(
            records(&served).accepted_turn_id().is_some(),
            "`http` 提交必须被接受"
        );
        assert_eq!(
            http.captured_requests().len(),
            1,
            "`http` provider 应恰好收到一次请求"
        );
        assert!(
            https.captured_requests().is_empty(),
            "`https` provider 仍不得收到请求"
        );
        evidence.record_captures(http.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
/// 启动时的 `SSL_CERT_FILE` 里没有一张证书：没有可用的根证书，`https` provider 的提交在接受前
/// 按配置错误拒绝（`http` 端点照常放行，见 `oversized_startup_roots_reject_https_provider_at_submit_and_serve_http`）。
fn no_usable_startup_tls_root_rejects_https_submissions_before_acceptance() {
    let (world, spec) = direct_world("background-no-tls-root", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let root = world.root().join("invalid-ca.pem");
        std::fs::write(&root, "invalid certificate").unwrap();
        world.set_env("SSL_CERT_FILE", root.display().to_string());
        let directory = world.home().join(".lattice");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("providers.toml"), "[metadata]\nauto_refresh=false\n[providers.local]\nprotocol='chat-completions'\nbase_url='https://localhost:9/v1'\nauth='none'\n").unwrap();
        world
            .start_daemon()
            .expect("显式启动无可用根证书环境的后台");
        let out = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "no roots", "--agent", "background"],
        );
        assert!(
            out.is_clean_exit(2),
            "无可用根证书时 `https` provider 提交应以退出码 2 退出"
        );
        let sent = records(&out);
        let error = sent.rejection().unwrap();
        assert_eq!(
            error.kind, "bad_request",
            "应作为配置错误返回 `bad_request`"
        );
        assert!(
            error.message.contains("configuration error")
                && error.message.contains("no usable TLS root"),
            "拒绝信息必须说明缺少可用根证书：{}",
            error.message
        );
        assert!(sent.accepted_turn_id().is_none());
    }));
    support::finish_recorded(evidence, result);
}

#[cfg(feature = "fake-tools")]
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
            "\n[llm.retry]\nmax_retries=3\n",
        );
        world.set_env(TEST_TOOLS, "read_note");
        let budget = world.budget(&spec);
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            world.start_daemon().expect("显式启动带测试工具环境的后台");
            let (server, gate) = wire_gated_fake_llm(
                &world,
                "background",
                support::sequence(vec![
                    tool_call("note", "read_note", r#"{"action":"read"}"#),
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
                    "[metadata]\nauto_refresh=false\n"
                } else {
                    "[metadata]\nauto_refresh=false\n[providers.local]\nprotocol='sk-invalid-config-diagnostic-a19bd57f'\n"
                },
            )
            .unwrap();
            gate.release(wait(&budget)).unwrap();
            let out = front.wait(Some(wait(&budget))).unwrap();
            evidence.record_command("轮次进行中配置失效后的发送结果", &out);
            assert!(out.is_clean_exit(1));
            let turn = closed(&world, &budget, &mut evidence, &accepted_turn(&out));
            assert_eq!(turn["steps"].as_array().unwrap().len(), 2);
            assert_eq!(turn["steps"][0]["slots"][0]["outcome"], "succeeded");
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
    let (world, spec) = direct_world("background-history-pages", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
        let server = wire_fake_llm(&world, "background", SseScript::plain("turn reply"));
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "background");
        // 12 轮单步对话：第 n 轮的用户消息是节点 2n-1，步骤是节点 2n。
        let turns = 12_u64;
        for n in 1..=turns {
            let sent = send.send(&format!("turn {n}")).unwrap();
            evidence.record_command(format!("第 {n} 轮发送"), &sent.command);
            assert_clean_send(&sent).unwrap();
            assert_accepted_and_closed(&sent).unwrap();
            // `session send --json` 只输出本轮：一条用户消息与一个步骤。
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
        // 往上翻页：以已装入的最早一轮的用户消息节点为 `before`，余下轮数不足所请求页大小。
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
        // 分页边界只落在用户消息节点之前：三轮正好是六个节点，首条为用户消息节点。
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
            "路径替换为截至节点 12 的新主链",
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
    let (world, spec) = direct_world("background-vanished-owner", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
        let (server, gate) = wire_gated_fake_llm(&world, "background", SseScript::plain("next"));
        let pgid = world.daemon_pgid().expect("自有后台进程组");
        assert_eq!(world.daemon_pids().unwrap(), vec![pgid]);
        let front = command(
            &world,
            &budget,
            &["--json", "run", "vanish", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        let (sid, tid) = running_turns(&world, &budget, &mut evidence)[0].clone();
        let killed = world.kill_daemon().expect("请求期间强制终止自有后台");
        evidence.record_command("请求执行期间向后台发送 SIGKILL", &killed);
        assert_forced_kill(&killed);
        let lost = front.wait(None).unwrap();
        evidence.record_command("前端检测到后台进程消失", &lost);
        assert!(lost.is_clean_exit(1), "{}", lost.summary());
        world
            .start_daemon()
            .expect("自有后台被强制终止后显式重启后台");
        let stopped = detail(&world, &budget, &mut evidence, &tid);
        assert_ne!(stopped["summary"]["progress"]["progress"], "closed");
        // 重新打开会话：会话头直接说明当前位置那一轮没有 `owner`，不必先观察那一轮。
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
        // 用户直接做下一件事：新一轮被接受并执行，旧轮次的节点保持原样。
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

#[cfg(feature = "fake-tools")]
#[test]
fn turn_and_session_identity_conflicts_are_rejected_without_dispatch() {
    let (world, spec) = direct_world("background-global-turn-identity", "background", "");
    world.set_env(TEST_TOOLS, "write_marker");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动带测试工具环境的后台");
        let marker_path = world.root().join("identity-tool.marker");
        let server = wire_fake_llm(
            &world,
            "background",
            SseScript::Cycle {
                scripts: vec![
                    tool_call(
                        "marker",
                        "write_marker",
                        &json!({
                            "action": "append_marker",
                            "path": marker_path,
                            "line": "dispatched\n",
                        })
                        .to_string(),
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
        let a_sid = a["session_id"].as_str().unwrap();
        let b_sid = b["session_id"].as_str().unwrap();
        let submit = |turn: &str, session_id: &str| {
            ipc(
                &world,
                wait(&budget),
                submit_request(&world, turn, session_id, "alpha"),
            )
        };
        // 请求身份表自上而下核对，先符合的一行决定结果：`TurnId` 先于会话存在性。
        let used_elsewhere = submit(&first_turn, b_sid);
        assert_eq!(
            conflict(&used_elsewhere),
            Some("turn_id_used"),
            "{used_elsewhere}"
        );
        let used_missing = submit(&first_turn, NEW_SESSION);
        assert_eq!(
            conflict(&used_missing),
            Some("turn_id_used"),
            "{used_missing}"
        );
        let exists = ipc(
            &world,
            wait(&budget),
            configure_request(
                &world,
                json!({"new": a_sid}),
                Some("background"),
                None,
                None,
            ),
        );
        assert_eq!(conflict(&exists), Some("session_exists"), "{exists}");
        let missing = submit(THIRD, NEW_SESSION);
        assert_eq!(missing["value"]["kind"], "not_found", "{missing}");
        let project = world.root().join("different-project");
        std::fs::create_dir(&project).unwrap();
        gix::init(&project).unwrap();
        let mut elsewhere = submit_request(&world, THIRD, a_sid, "alpha");
        elsewhere["request"]["workspace"] =
            json!(support::path_bytes(&project.canonicalize().unwrap()));
        let binding = ipc(&world, wait(&budget), elsewhere);
        assert_eq!(conflict(&binding), Some("binding_mismatch"), "{binding}");
        // 被拒绝的请求都没有建立轮次：`THIRD` 仍查不到，会话数不变。
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
                &json!({"turn_used_elsewhere":used_elsewhere,"turn_used_missing":used_missing,
                    "session_exists":exists,"missing":missing,"binding":binding}),
            )
            .unwrap();
        archive_state(&world, &budget, &mut evidence, &[&first_turn, &second_turn]);
        evidence.record_json("parameters", &json!({"turns":[first_turn,second_turn],"projects":2,"script":"two write_marker/final Turns; rejected identities"})).unwrap();
        assert_eq!(detail(&world, &budget, &mut evidence, &first_turn), a);
        assert_eq!(detail(&world, &budget, &mut evidence, &second_turn), b);
        assert_eq!(server.captured_requests().len(), 4);
        assert_eq!(marker_lines(&marker_path), vec!["dispatched", "dispatched"]);
        let inv_a = &a["steps"][0]["slots"][0]["invocation_id"];
        let inv_b = &b["steps"][0]["slots"][0]["invocation_id"];
        assert_ne!(inv_a, inv_b);
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

// 后续提交与静态 `head` 对比都等待本轮关闭且后台交出执行权。
fn closed_without_owner(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    tid: &str,
) -> Value {
    let turn = closed(world, budget, evidence, tid);
    let sid = turn["session_id"].as_str().unwrap();
    let mut watcher = Watcher::open(world, wait(budget), sid);
    watcher.until(
        Instant::now() + wait(budget),
        "轮次收束且占有方已释放",
        |view| view.head.as_ref().is_some_and(|head| !head.owner) && turn_closed(view, tid),
    );
    turn
}

#[test]
fn public_choices_persist_and_model_changes_use_supported_fallbacks() {
    use lattice_system_harness::{ProviderFixture, ProviderTarget};
    let (world, spec) = direct_world("background-model-effort-copy", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
        let server = wire_fake_llm(&world, "background", SseScript::plain("chosen reply"));
        let other = ProviderFixture {
            target: ProviderTarget::Custom("other"),
            ..ProviderFixture::chat_completions("a")
        };
        world.wire_provider(other, &server, FAKE_API_KEY).unwrap();
        let path = world.home().join(".lattice/providers.toml");
        let mut registry: toml::Table =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        registry["providers"]
            .as_table_mut()
            .unwrap()
            .get_mut("local")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .insert("catalog".into(), "copy-fixture".into());
        let base = toml::to_string(&registry).unwrap();
        let cache = world.home().join(".lattice/cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join("models.json"),
            serde_json::to_vec(
                &json!({"copy-fixture":{"models":{"old":{"id":"old","status":"deprecated"}}}}),
            )
            .unwrap(),
        )
        .unwrap();
        let metadata = "\n[providers.local.models.a]\neffort=['off','low','high']\n[providers.local.models.b]\neffort=['high']\n[providers.local.models.both]\neffort=['low','high']\n[providers.local.models.c]\neffort=['low']\n[providers.other.models.a]\neffort=['medium']\n";
        std::fs::write(&path, format!("{base}{metadata}")).unwrap();
        world.write_project_file(".lattice/agents/background/agent.toml", "schema_version=1\n[agent]\nname='background'\neffort='low'\n[prompt]\nsystem='copy prompt'\n").unwrap();
        // `configure(New)` 允许模型为空并保存基准档位；未选模型前 `submit` 被配置拒绝。
        let draft = ipc(
            &world,
            wait(&budget),
            configure_request(
                &world,
                json!({"new":NEW_SESSION}),
                Some("background"),
                None,
                None,
            ),
        );
        assert_eq!(draft["response"], "configure", "{draft}");
        assert_eq!(draft["value"]["head"]["model"], Value::Null);
        assert_eq!(draft["value"]["head"]["provider"], Value::Null);
        assert_eq!(draft["value"]["head"]["effort"], "low");
        let rejected = ipc(
            &world,
            wait(&budget),
            submit_request(&world, FIRST, NEW_SESSION, "missing model"),
        );
        assert_eq!(rejected["response"], "error");
        assert_eq!(rejected["value"]["kind"], "bad_request");
        assert!(
            rejected["value"]["message"]
                .as_str()
                .unwrap()
                .contains("model"),
            "{rejected}"
        );
        assert!(server.captured_requests().is_empty());
        let configured = ipc(
            &world,
            wait(&budget),
            configure_request(
                &world,
                json!({"existing":NEW_SESSION}),
                None,
                Some("local/a"),
                Some("high"),
            ),
        );
        assert_eq!(configured["response"], "configure", "{configured}");
        let reply = ipc(
            &world,
            wait(&budget),
            submit_request(&world, FIRST, NEW_SESSION, "chosen"),
        );
        assert_eq!(reply["response"], "acceptance", "{reply}");
        assert_eq!(
            closed_without_owner(&world, &budget, &mut evidence, FIRST)["summary"]["outcome"],
            "completed"
        );
        let before = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":NEW_SESSION}),
        );
        let previous_log = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "log", NEW_SESSION],
        );
        let count = server.captured_requests().len();
        std::fs::write(
            &path,
            format!(
                "{base}{}",
                metadata.replace("['off','low','high']", "['low']")
            ),
        )
        .unwrap();
        // 同模型的已存档位失效时应拒绝（无论是直接提交还是经 `configure` 指定同一模型），不能静默改用默认 `low`。
        for (label, request) in [
            (
                "submit",
                submit_request(
                    &world,
                    "00000000-0000-4000-8000-fffffffffff2",
                    NEW_SESSION,
                    "saved effort no longer supported",
                ),
            ),
            (
                "same-model-configure",
                configure_request(
                    &world,
                    json!({"existing":NEW_SESSION}),
                    None,
                    Some("local/a"),
                    None,
                ),
            ),
        ] {
            let rejected = ipc(&world, wait(&budget), request);
            assert_eq!(rejected["response"], "error", "{label}: {rejected}");
            assert_eq!(rejected["value"]["kind"], "bad_request");
            assert_eq!(server.captured_requests().len(), count);
            assert_eq!(
                ipc(
                    &world,
                    wait(&budget),
                    json!({"operation":"head", "session_id":NEW_SESSION}),
                ),
                before
            );
            assert_eq!(
                read(
                    &world,
                    &budget,
                    &mut evidence,
                    &["--json", "session", "log", NEW_SESSION],
                ),
                previous_log
            );
        }
        std::fs::write(&path, format!("{base}{metadata}")).unwrap();
        std::fs::remove_dir_all(world.project_dir().join(".lattice/agents/background")).unwrap();
        let cases = [
            (None, None, "a", Some("high")),
            (Some("local/b"), None, "b", Some("high")),
            (Some("local/both"), None, "both", Some("high")),
            (Some("local/c"), None, "c", Some("low")),
            (Some("local/unknown"), None, "unknown", None),
        ];
        let mut ordinal = 1_u64;
        let mut submit = |model: Option<&str>,
                          effort: Option<&str>,
                          expected_model: &str,
                          expected_effort: Option<&str>,
                          provider: &str| {
            ordinal += 1;
            let mut args = vec![
                "--json",
                "session",
                "send",
                "next choice",
                "--session",
                NEW_SESSION,
            ];
            if let Some(model) = model {
                args.extend(["--model", model]);
            }
            if let Some(effort) = effort {
                args.extend(["--effort", effort]);
            }
            let output = run(&world, &budget, &mut evidence, &args);
            assert!(output.is_clean_exit(0), "{}", output.summary());
            assert_accepted_and_closed(&records(&output)).unwrap();
            let tid = accepted_turn(&output);
            assert_eq!(
                closed_without_owner(&world, &budget, &mut evidence, &tid)["summary"]["outcome"],
                "completed"
            );
            let head = ipc(
                &world,
                wait(&budget),
                json!({"operation":"head", "session_id":NEW_SESSION}),
            );
            assert_eq!(head["value"]["model"], expected_model);
            assert_eq!(head["value"]["provider"], provider);
            assert_eq!(head["value"]["effort"], json!(expected_effort));
            assert!(head["value"]["settings_version"].as_str().is_some());
            let capture = server.captured_requests().pop().unwrap();
            assert_eq!(capture.raw["model"], expected_model);
            assert_eq!(
                capture.raw["reasoning_effort"],
                json!(expected_effort.map(|effort| if effort == "off" { "none" } else { effort }))
            );
            let mut watcher = Watcher::open(&world, wait(&budget), NEW_SESSION);
            let view = watcher.initial();
            let user = &view.entries[&(2 * ordinal - 1)];
            assert_eq!(user.raw["content"]["effort"], json!(expected_effort));
        };
        for (model, effort, expected_model, expected_effort) in cases {
            submit(model, effort, expected_model, expected_effort, "local");
        }
        // 元数据开始支持默认档位后，同模型的空值仍保持为空。
        std::fs::write(
            &path,
            format!("{base}{metadata}\n[providers.local.models.unknown]\neffort=['low']\n"),
        )
        .unwrap();
        submit(None, None, "unknown", None, "local");
        submit(Some("local/unknown"), None, "unknown", None, "local");
        submit(Some("local/a"), None, "a", Some("low"), "local");
        submit(None, Some("off"), "a", Some("off"), "local");
        submit(None, None, "a", Some("off"), "local");
        submit(Some("local/both"), None, "both", Some("low"), "local");
        submit(Some("local/a"), Some("off"), "a", Some("off"), "local");
        submit(Some("other/a"), None, "a", None, "other");
        submit(
            Some("other/a"),
            Some("medium"),
            "a",
            Some("medium"),
            "other",
        );
        submit(Some("local/org/model"), None, "org/model", None, "local");
        submit(Some("local/old"), None, "old", None, "local");
        let before = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":NEW_SESSION}),
        );
        let count = server.captured_requests().len();
        let previous_log = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "log", NEW_SESSION],
        );
        let bad = configure_request(
            &world,
            json!({"existing":NEW_SESSION}),
            None,
            None,
            Some("max"),
        );
        let rejected = ipc(&world, wait(&budget), bad);
        assert_eq!(rejected["response"], "error");
        assert_eq!(rejected["value"]["kind"], "bad_request");
        assert_eq!(server.captured_requests().len(), count);
        assert_eq!(
            ipc(
                &world,
                wait(&budget),
                json!({"operation":"head", "session_id":NEW_SESSION})
            ),
            before
        );
        assert_eq!(
            read(
                &world,
                &budget,
                &mut evidence,
                &["--json", "session", "log", NEW_SESSION]
            ),
            previous_log
        );
        let rejected_cli = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "unsupported effort",
                "--session",
                NEW_SESSION,
                "--effort",
                "max",
            ],
        );
        assert!(rejected_cli.is_clean_exit(2));
        assert_eq!(
            records(&rejected_cli).rejection().unwrap().kind,
            "bad_request"
        );
        assert_eq!(server.captured_requests().len(), count);
        assert_eq!(
            ipc(
                &world,
                wait(&budget),
                json!({"operation":"head", "session_id":NEW_SESSION})
            ),
            before
        );
        assert_eq!(
            read(
                &world,
                &budget,
                &mut evidence,
                &["--json", "session", "log", NEW_SESSION]
            ),
            previous_log
        );
        let mut watcher = Watcher::open(&world, wait(&budget), NEW_SESSION);
        let view = watcher.initial();
        let head = view.head.as_ref().unwrap();
        assert_eq!(head.model.as_deref(), Some("old"));
        assert_eq!(head.provider.as_deref(), Some("local"));
        assert_eq!(head.effort, None);
        world.write_project_file(".lattice/agents/background/agent.toml", "schema_version=1\n[agent]\nname='background'\nmodel='local/a'\neffort='low'\n[prompt]\nsystem='rebuilt prompt'\n").unwrap();
        let rebuilt_cfg = ipc(
            &world,
            wait(&budget),
            configure_request(
                &world,
                json!({"existing":NEW_SESSION}),
                Some("background"),
                Some("local/b"),
                None,
            ),
        );
        assert_eq!(rebuilt_cfg["response"], "configure", "{rebuilt_cfg}");
        let rebuilt_id = "00000000-0000-4000-8000-fffffffffff1";
        let rebuilt = submit_request(&world, rebuilt_id, NEW_SESSION, "new agent baseline");
        assert_eq!(
            ipc(&world, wait(&budget), rebuilt)["response"],
            "acceptance"
        );
        closed_without_owner(&world, &budget, &mut evidence, rebuilt_id);
        let rebuilt_head = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":NEW_SESSION}),
        );
        assert_eq!(rebuilt_head["value"]["model"], "b");
        assert_eq!(rebuilt_head["value"]["effort"], Value::Null);
        let capture = server.captured_requests().pop().unwrap();
        assert_eq!(capture.raw["messages"][0]["content"], "rebuilt prompt");
        assert!(capture.raw.get("reasoning_effort").is_none());
        let source_path = world
            .project_dir()
            .join(".lattice/agents/background/agent.toml");
        let source = std::fs::read(&source_path).unwrap();
        let explicit = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "explicit agent and choices",
                "--session",
                NEW_SESSION,
                "--agent",
                "background",
                "--model",
                "local/a",
                "--effort",
                "off",
            ],
        );
        assert!(explicit.is_clean_exit(0));
        let head = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":NEW_SESSION}),
        );
        assert_eq!(head["value"]["model"], "a");
        assert_eq!(head["value"]["effort"], "off");
        assert_eq!(
            server.captured_requests().last().unwrap().raw["reasoning_effort"],
            "none"
        );
        assert_eq!(std::fs::read(&source_path).unwrap(), source);
        evidence.record_captures(server.captured_requests());
        evidence.record_json("choices-head", &before).unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[cfg(feature = "fake-tools")]
#[test]
fn cli_off_survives_tool_retry_restart_and_provider_failure() {
    use lattice_system_harness::{FakeLlmServer, ProviderFixture, ProviderTarget};
    let (world, spec) = direct_world("cli-luna-off-tools", "background", "");
    world.set_env(TEST_TOOLS, "read_fixture");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let read = || {
            tool_call(
                "read-mark",
                "read_fixture",
                r#"{"action":"read","value":{"content":"luna-off-226"}}"#,
            )
        };
        let server = FakeLlmServer::start(support::sequence(vec![
            SseScript::HttpError {
                status: 503,
                body: "retry once".into(),
            },
            read(),
            SseScript::plain("file marker: luna-off-226"),
            read(),
            SseScript::plain("inherited marker: luna-off-226"),
            SseScript::HttpError {
                status: 400,
                body: "accepted provider failure".into(),
            },
            SseScript::plain("saved low"),
            SseScript::HttpError {
                status: 400,
                body: "explicit off provider failure".into(),
            },
            SseScript::plain("still off after failure"),
        ]))
        .unwrap();
        let fixture = ProviderFixture {
            target: ProviderTarget::Custom("openai-chat"),
            ..ProviderFixture::chat_completions("gpt-6-luna")
        };
        world.start_daemon().expect("显式启动带测试工具环境的后台");
        world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
        let configuration = world.home().join(".lattice/providers.toml");
        let mut providers: toml::Table =
            toml::from_str(&std::fs::read_to_string(&configuration).unwrap()).unwrap();
        providers["providers"]["openai-chat"]
            .as_table_mut()
            .unwrap()
            .insert("catalog".into(), "openai".into());
        std::fs::write(&configuration, toml::to_string(&providers).unwrap()).unwrap();
        let cache = world.home().join(".lattice/cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("models.json"), serde_json::to_vec(&json!({"openai":{"models":{
            "gpt-6-luna":{"id":"gpt-6-luna", "reasoning_options":[{"type":"effort","values":["none","low"]}]}
        }}})).unwrap()).unwrap();
        let profile = "schema_version=1\n[agent]\nname='background'\neffort='low'\n[prompt]\nsystem='Read fixture.txt using the tool.'\n";
        world
            .write_project_file(".lattice/agents/background/agent.toml", profile)
            .unwrap();
        let first = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "read fixture.txt",
                "--agent",
                "background",
                "--model",
                "openai-chat/gpt-6-luna",
                "--effort",
                "off",
            ],
        );
        assert!(first.is_clean_exit(0), "{}", first.summary());
        assert_accepted_and_closed(&records(&first)).unwrap();
        let sid = records(&first).accepted_session_id().unwrap().to_owned();
        let first_turn =
            closed_without_owner(&world, &budget, &mut evidence, &accepted_turn(&first));
        assert_eq!(
            first_turn["steps"][0]["attempts"].as_array().unwrap().len(),
            2
        );
        assert_eq!(first_turn["steps"][0]["slots"][0]["outcome"], "succeeded");
        assert_eq!(server.captured_requests().len(), 3);
        world.restart_daemon().expect("显式重启后台");
        let inherited = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "send", "read again", "--session", &sid],
        );
        assert!(inherited.is_clean_exit(0), "{}", inherited.summary());
        let inherited_turn =
            closed_without_owner(&world, &budget, &mut evidence, &accepted_turn(&inherited));
        assert_eq!(
            inherited_turn["steps"][0]["slots"][0]["outcome"],
            "succeeded"
        );
        assert_eq!(server.captured_requests().len(), 5);
        let failure = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "provider fails after acceptance",
                "--session",
                &sid,
            ],
        );
        assert!(failure.is_clean_exit(1), "{}", failure.summary());
        let failed_turn =
            closed_without_owner(&world, &budget, &mut evidence, &accepted_turn(&failure));
        assert_eq!(failed_turn["summary"]["outcome"], "failed");
        assert_eq!(
            failed_turn["steps"][0]["attempts"][0]["end"]["cause"],
            json!({"cause":"provider_error", "retryable":false})
        );
        let mut watcher = Watcher::open(&world, wait(&budget), &sid);
        let view = watcher.initial();
        let head = view.head.as_ref().unwrap();
        assert_eq!(head.provider.as_deref(), Some("openai-chat"));
        assert_eq!(head.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(head.effort.as_deref(), Some("off"));
        for output in [&first, &inherited, &failure] {
            let sent = records(output);
            let id = accepted_turn(output);
            let user = view
                .entries
                .values()
                .find(|entry| {
                    entry.raw["kind"] == "user_message" && entry.raw["content"]["turn_id"] == id
                })
                .unwrap();
            assert_eq!(user.raw["content"]["model"], "gpt-6-luna");
            assert_eq!(user.raw["content"]["provider"], "openai-chat");
            assert_eq!(user.raw["content"]["effort"], "off");
            assert!(sent.rejection().is_none());
        }
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 6);
        for capture in &captures {
            assert_eq!(capture.raw["model"], "gpt-6-luna");
            assert_eq!(capture.raw["reasoning_effort"], "none");
            assert!(capture.tool_names.iter().any(|name| name == "read_fixture"));
            assert!(capture.raw.get("enable_thinking").is_none());
        }
        for index in [2, 4] {
            let messages = captures[index].raw["messages"].as_array().unwrap();
            let result = messages
                .iter()
                .rev()
                .find(|message| message["role"] == "tool")
                .unwrap();
            assert!(result["content"].to_string().contains("luna-off-226"));
        }
        let low = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "save low",
                "--session",
                &sid,
                "--effort",
                "low",
            ],
        );
        assert!(low.is_clean_exit(0), "{}", low.summary());
        closed_without_owner(&world, &budget, &mut evidence, &accepted_turn(&low));
        let low_head = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":sid}),
        );
        assert_eq!(low_head["value"]["effort"], "low");
        let failed_off = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "run",
                "explicit off fails after acceptance",
                "--session",
                &sid,
                "--effort",
                "off",
            ],
        );
        assert!(failed_off.is_clean_exit(1), "{}", failed_off.summary());
        let failed_off_turn =
            closed_without_owner(&world, &budget, &mut evidence, &accepted_turn(&failed_off));
        assert_eq!(failed_off_turn["summary"]["outcome"], "failed");
        assert_eq!(
            failed_off_turn["steps"][0]["attempts"][0]["end"]["cause"],
            json!({"cause":"provider_error", "retryable":false})
        );
        let failed_off_head = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":sid}),
        );
        assert_eq!(failed_off_head["value"]["effort"], "off");
        world.restart_daemon().expect("显式重启后台");
        let continued = run(
            &world,
            &budget,
            &mut evidence,
            &[
                "--json",
                "session",
                "send",
                "inherit off after failure and restart",
                "--session",
                &sid,
            ],
        );
        assert!(continued.is_clean_exit(0), "{}", continued.summary());
        closed_without_owner(&world, &budget, &mut evidence, &accepted_turn(&continued));
        let mut watcher = Watcher::open(&world, wait(&budget), &sid);
        let view = watcher.initial();
        assert_eq!(view.head.as_ref().unwrap().effort.as_deref(), Some("off"));
        for (output, effort) in [(&low, "low"), (&failed_off, "off"), (&continued, "off")] {
            let id = accepted_turn(output);
            let user = view
                .entries
                .values()
                .find(|entry| {
                    entry.raw["kind"] == "user_message" && entry.raw["content"]["turn_id"] == id
                })
                .unwrap();
            assert_eq!(user.raw["content"]["effort"], effort);
        }
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 9);
        for (index, effort) in [(6, "low"), (7, "none"), (8, "none")] {
            assert_eq!(captures[index].raw["reasoning_effort"], effort);
        }
        assert_eq!(
            std::fs::read(
                world
                    .project_dir()
                    .join(".lattice/agents/background/agent.toml")
            )
            .unwrap(),
            profile.as_bytes()
        );
        evidence.record_captures(captures);
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn session_settings_survive_sources_and_explicit_same_agent_refreshes_profile() {
    let (world, spec) = direct_world("background-copy-sources", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
        let first = wire_fake_llm(&world, "background", SseScript::plain("profile reply"));
        world
            .write_project_file(
                ".lattice/agents/background/prompt.md",
                "original external prompt",
            )
            .unwrap();
        let original = "schema_version=1\n[agent]\nname='background'\nmodel='local/background'\n[prompt]\nsystem_path='prompt.md'\n";
        world
            .write_project_file(".lattice/agents/background/agent.toml", original)
            .unwrap();
        let mut ordinal = 0_u64;
        let mut send = |fresh: bool, agent: Option<&str>, sid: &str, prompt: &str| {
            ordinal += 1;
            let tid = format!("00000000-0000-4000-8000-{ordinal:012x}");
            if fresh || agent.is_some() {
                let session = if fresh {
                    json!({"new":sid})
                } else {
                    json!({"existing":sid})
                };
                let configured = ipc(
                    &world,
                    wait(&budget),
                    configure_request(&world, session, agent, None, None),
                );
                assert_eq!(configured["response"], "configure", "{configured}");
            }
            let reply = ipc(
                &world,
                wait(&budget),
                submit_request(&world, &tid, sid, "check profile"),
            );
            assert_eq!(reply["response"], "acceptance", "{reply}");
            let turn = closed_without_owner(&world, &budget, &mut evidence, &tid);
            assert_eq!(turn["summary"]["outcome"], "completed");
            let mut watcher = Watcher::open(&world, wait(&budget), sid);
            let view = watcher.initial();
            assert_eq!(
                view.head.as_ref().unwrap().model.as_deref(),
                Some(if prompt == "original external prompt" {
                    "background"
                } else {
                    "refreshed"
                })
            );
        };
        send(
            true,
            Some("background"),
            NEW_SESSION,
            "original external prompt",
        );
        let second = wire_fake_llm(&world, "background", SseScript::plain("profile reply"));
        world
            .write_project_file(
                ".lattice/agents/background/prompt.md",
                "changed external prompt",
            )
            .unwrap();
        world
            .write_project_file(
                ".lattice/agents/background/agent.toml",
                &original.replace("local/background", "local/refreshed"),
            )
            .unwrap();
        send(false, None, NEW_SESSION, "original external prompt");
        std::fs::remove_dir_all(world.project_dir().join(".lattice/agents/background")).unwrap();
        send(false, None, NEW_SESSION, "original external prompt");
        let before = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":NEW_SESSION}),
        );
        let missing = ipc(
            &world,
            wait(&budget),
            configure_request(
                &world,
                json!({"existing":NEW_SESSION}),
                Some("background"),
                None,
                None,
            ),
        );
        assert_eq!(missing["response"], "error");
        assert_eq!(
            ipc(
                &world,
                wait(&budget),
                json!({"operation":"head", "session_id":NEW_SESSION})
            ),
            before
        );
        world
            .write_project_file(
                ".lattice/agents/background/agent.toml",
                "schema_version=1\n[agent]\nname='background'\nmodel='local/refreshed'\n[prompt]\nsystem='refreshed prompt'\n",
            )
            .unwrap();
        send(false, Some("background"), NEW_SESSION, "refreshed prompt");
        send(
            true,
            Some("background"),
            "5e550000000000000000000000000002",
            "refreshed prompt",
        );
        let first_captures = first.captured_requests();
        assert_eq!(first_captures.len(), 1);
        assert_eq!(
            first_captures[0].raw["messages"][0]["content"],
            "original external prompt"
        );
        assert_eq!(first_captures[0].raw["model"], "background");
        let captures = second.captured_requests();
        assert_eq!(captures.len(), 4);
        for (index, capture) in captures.iter().enumerate() {
            assert_eq!(
                capture.raw["messages"][0]["content"],
                if index < 2 {
                    "original external prompt"
                } else {
                    "refreshed prompt"
                }
            );
            assert_eq!(
                capture.raw["model"],
                if index < 2 { "background" } else { "refreshed" }
            );
        }
        evidence.record_captures(first_captures);
        evidence.record_captures(captures);
    }));
    support::finish_recorded(evidence, result);
}

#[cfg(feature = "fake-tools")]
#[test]
fn accepted_effort_survives_retry_and_tools_while_global_budget_reloads() {
    let (world, spec) = direct_world("background-effort-reload", "background", "");
    world.set_env(TEST_TOOLS, "read_note");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动带测试工具环境的后台");
        let script = SseScript::Sequence {
            scripts: vec![
                SseScript::HttpError {
                    status: 503,
                    body: "retry later".into(),
                },
                tool_call("first-read", "read_note", r#"{"action":"read"}"#),
                tool_call("second-read", "read_note", r#"{"action":"read"}"#),
                SseScript::plain("must not run"),
            ],
        };
        let (server, gate) = wire_gated_fake_llm(&world, "background", script);
        let path = world.home().join(".lattice/providers.toml");
        let mut config: toml::Table =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let provider = config
            .get_mut("providers")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .get_mut("local")
            .unwrap()
            .as_table_mut()
            .unwrap();
        provider.insert("catalog".into(), "custom".into());
        provider.insert(
            "features".into(),
            toml::Value::Table(
                toml::from_str(
                    "thinking_budget='thinking-budget'\nthinking_toggle='enable-thinking'",
                )
                .unwrap(),
            ),
        );
        std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
        let cache = world.home().join(".lattice/cache");
        std::fs::create_dir_all(&cache).unwrap();
        let metadata = |output| json!({"custom":{"models":{"background":{"id":"background","limit":{"output":output},"reasoning_options":[{"type":"toggle"},{"type":"budget_tokens"}]}}}});
        std::fs::write(
            cache.join("models.json"),
            serde_json::to_vec(&metadata(65536)).unwrap(),
        )
        .unwrap();
        let configured = ipc(
            &world,
            wait(&budget),
            configure_request(
                &world,
                json!({"new":NEW_SESSION}),
                Some("background"),
                None,
                Some("high"),
            ),
        );
        assert_eq!(configured["response"], "configure", "{configured}");
        let request = submit_request(&world, FIRST, NEW_SESSION, "retry and tools");
        assert_eq!(
            ipc(&world, wait(&budget), request)["response"],
            "acceptance"
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        assert_eq!(server.captured_requests()[0].raw["thinking_budget"], 16384);
        std::fs::write(
            cache.join("models.json"),
            serde_json::to_vec(&metadata(24576)).unwrap(),
        )
        .unwrap();
        gate.release_next(wait(&budget)).unwrap();
        gate.wait_until_stalled(2, wait(&budget)).unwrap();
        assert_eq!(server.captured_requests()[1].raw["thinking_budget"], 12288);
        gate.release_next(wait(&budget)).unwrap();
        gate.wait_until_stalled(3, wait(&budget)).unwrap();
        assert_eq!(server.captured_requests()[2].raw["thinking_budget"], 12288);
        config["providers"]["local"]["features"]["thinking_budget"] = "none".into();
        std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
        gate.release_next(wait(&budget)).unwrap();
        let turn = closed_without_owner(&world, &budget, &mut evidence, FIRST);
        assert_eq!(turn["summary"]["outcome"], "failed");
        assert_eq!(turn["steps"].as_array().unwrap().len(), 3);
        assert_eq!(turn["steps"][0]["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(turn["steps"][0]["slots"][0]["outcome"], "succeeded");
        assert_eq!(turn["steps"][1]["slots"][0]["outcome"], "succeeded");
        assert_eq!(server.captured_requests().len(), 3);
        let head = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":NEW_SESSION}),
        );
        assert_eq!(head["value"]["effort"], "high");
        let mut watcher = Watcher::open(&world, wait(&budget), NEW_SESSION);
        assert_eq!(
            watcher.initial().entries[&1].raw["content"]["effort"],
            "high"
        );
        evidence.record_captures(server.captured_requests());
        evidence
            .record_json("effort-after-global-change", &head)
            .unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn draft_without_model_survives_restart_and_enforces_settings_version() {
    const FOURTH: &str = "00000000-0000-4000-8000-000000000004";
    let (world, spec) = direct_world("background-draft-restart-settings", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
        let server = wire_fake_llm(&world, "background", SseScript::plain("draft model reply"));
        // 改写 `agent` 配置使其不含 `[agent].model`，经原始进程间通信创建无模型的草稿会话。
        world
            .write_project_file(
                ".lattice/agents/background/agent.toml",
                "schema_version = 1\n[agent]\nname = \"background\"\n[prompt]\nsystem = \"draft without model\"\n",
            )
            .unwrap();
        let first_epoch =
            ipc(&world, wait(&budget), json!({"operation":"status"}))["value"]["epoch"]
                .as_str()
                .unwrap()
                .to_owned();
        let configured = ipc(
            &world,
            wait(&budget),
            configure_request(
                &world,
                json!({"new":NEW_SESSION}),
                Some("background"),
                None,
                None,
            ),
        );
        assert_eq!(configured["response"], "configure", "{configured}");
        let initial_head = &configured["value"]["head"];
        assert_eq!(initial_head["session_id"], NEW_SESSION);
        assert_eq!(initial_head["position"], 0);
        assert_eq!(initial_head["agent"], "background");
        assert_eq!(initial_head["model"], Value::Null);
        assert_eq!(initial_head["provider"], Value::Null);
        assert_eq!(initial_head["effort"], Value::Null);
        let stale_version = initial_head["settings_version"]
            .as_str()
            .unwrap()
            .to_owned();

        // 显式重启后台：草稿仍在列表中（节点数为 1），会话头照常可读。
        world.restart_daemon().expect("显式重启后台");
        let listed = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "list"],
        );
        assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
        assert_eq!(listed[0]["session_id"], NEW_SESSION);
        assert_eq!(listed[0]["node_count"], 1);
        assert_eq!(listed[0]["current"], 0);
        assert_eq!(listed[0]["unclosed_turn"], Value::Null);
        let second_epoch =
            ipc(&world, wait(&budget), json!({"operation":"status"}))["value"]["epoch"]
                .as_str()
                .unwrap()
                .to_owned();
        assert_ne!(second_epoch, first_epoch, "后台重启后必须进入新的实例代");
        let reopened_head = ipc(
            &world,
            wait(&budget),
            json!({"operation":"head", "session_id":NEW_SESSION}),
        );
        assert_eq!(reopened_head["response"], "head", "{reopened_head}");
        assert_eq!(reopened_head["value"], *initial_head);

        // 未选模型直接提交：配置拒绝并提示先选择模型，不写节点，设置版本不变。
        let rejected = ipc(
            &world,
            wait(&budget),
            submit_request(&world, FIRST, NEW_SESSION, "missing model"),
        );
        assert_eq!(rejected["response"], "error", "{rejected}");
        assert_eq!(rejected["value"]["kind"], "bad_request", "{rejected}");
        assert!(
            rejected["value"]["message"]
                .as_str()
                .unwrap()
                .contains("choose one with /model or --model"),
            "未选模型提交应提示先选择模型：{rejected}"
        );
        assert_eq!(
            turn_lookup(&world, wait(&budget), FIRST)["value"]["kind"],
            "not_found"
        );
        assert!(server.captured_requests().is_empty());
        let list_after_reject = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "list"],
        );
        assert_eq!(list_after_reject[0]["node_count"], 1);
        let log_after_reject = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "log", NEW_SESSION],
        );
        assert_eq!(log_after_reject["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(
            ipc(
                &world,
                wait(&budget),
                json!({"operation":"head", "session_id":NEW_SESSION})
            ),
            reopened_head
        );

        // 为已有草稿选定模型后提交：本轮完成，用户消息节点记录所选模型。
        let updated = ipc(
            &world,
            wait(&budget),
            configure_request(
                &world,
                json!({"existing":NEW_SESSION}),
                None,
                Some("local/background"),
                None,
            ),
        );
        assert_eq!(updated["response"], "configure", "{updated}");
        assert_eq!(updated["value"]["head"]["model"], "background");
        assert_eq!(updated["value"]["head"]["provider"], "local");
        let current_version = updated["value"]["head"]["settings_version"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(current_version, stale_version);

        let accepted = ipc(
            &world,
            wait(&budget),
            submit_request(&world, SECOND, NEW_SESSION, "first turn with model"),
        );
        assert_eq!(accepted["response"], "acceptance", "{accepted}");
        assert_eq!(accepted["value"]["acceptance"], "accepted", "{accepted}");
        let completed = closed_without_owner(&world, &budget, &mut evidence, SECOND);
        assert_eq!(completed["summary"]["outcome"], "completed");
        let mut watcher = Watcher::open(&world, wait(&budget), NEW_SESSION);
        let view = watcher.initial();
        assert_eq!(view.entries[&1].raw["content"]["model"], "background");
        assert_eq!(view.entries[&1].raw["content"]["provider"], "local");
        assert_eq!(server.captured_requests().len(), 1);
        assert_eq!(server.captured_requests()[0].raw["model"], "background");

        // 携带过期的设置版本号提交返回 `settings_changed` 冲突且不写节点；换用当前版本提交成功。
        let mut stale_submit = submit_request(&world, THIRD, NEW_SESSION, "stale version turn");
        stale_submit["request"]["expected_settings"] = json!(stale_version);
        let stale_reply = ipc(&world, wait(&budget), stale_submit);
        assert_eq!(
            conflict(&stale_reply),
            Some("settings_changed"),
            "{stale_reply}"
        );
        assert_eq!(
            turn_lookup(&world, wait(&budget), THIRD)["value"]["kind"],
            "not_found"
        );
        assert_eq!(server.captured_requests().len(), 1);
        let log_after_conflict = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "log", NEW_SESSION],
        );
        assert_eq!(log_after_conflict["nodes"].as_array().unwrap().len(), 3);

        let mut fresh_submit = submit_request(&world, FOURTH, NEW_SESSION, "fresh version turn");
        fresh_submit["request"]["expected_settings"] = json!(current_version);
        let fresh_reply = ipc(&world, wait(&budget), fresh_submit);
        assert_eq!(fresh_reply["response"], "acceptance", "{fresh_reply}");
        assert_eq!(
            fresh_reply["value"]["acceptance"], "accepted",
            "{fresh_reply}"
        );
        let fresh_done = closed_without_owner(&world, &budget, &mut evidence, FOURTH);
        assert_eq!(fresh_done["summary"]["outcome"], "completed");
        assert_eq!(server.captured_requests().len(), 2);
        let first_entries = history(&world, wait(&budget), NEW_SESSION, 3, 1).unwrap();
        assert_eq!(first_entries[0]["content"]["model"], "background");
        assert_eq!(first_entries[0]["content"]["provider"], "local");
        let final_log = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "session", "log", NEW_SESSION],
        );
        assert_eq!(final_log["nodes"].as_array().unwrap().len(), 5);
        archive_state(&world, &budget, &mut evidence, &[SECOND, FOURTH]);
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

fn poll_daemon_status(
    world: &ScenarioWorld,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    predicate: impl Fn(&Value) -> bool,
    message: &str,
) -> Value {
    let deadline = Instant::now() + wait(budget);
    loop {
        let out = run(world, budget, evidence, &["--json", "daemon", "status"]);
        if out.is_clean_exit(0) {
            let status: Value = serde_json::from_slice(&out.stdout).unwrap();
            if predicate(&status) {
                return status;
            }
        }
        assert!(Instant::now() < deadline, "{message}");
        std::thread::yield_now();
    }
}

#[test]
fn sigterm_drains_inflight_turn_and_exits_zero() {
    let (world, spec) = direct_world("background-sigterm-drain", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world.start_daemon().expect("显式启动后台");
        let (server, gate) =
            wire_gated_fake_llm(&world, "background", SseScript::plain("drained reply"));
        let front = command(
            &world,
            &budget,
            &["--json", "run", "drain me", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        poll_daemon_status(
            &world,
            &budget,
            &mut evidence,
            |status| status["owners"] == 1,
            "后台 `owners` 未变为 1",
        );
        let (sid, tid) = running_turns(&world, &budget, &mut evidence)[0].clone();

        world.terminate_daemon().expect("向自有后台发送 `SIGTERM`");
        poll_daemon_status(
            &world,
            &budget,
            &mut evidence,
            |status| status["stopping"] == true,
            "收到 `SIGTERM` 后后台未进入 `stopping`",
        );
        assert!(
            !world.daemon_has_exited().unwrap(),
            "排空期间在途轮次尚未收尾，后台不应提前退出"
        );

        gate.release(wait(&budget)).unwrap();
        let front_done = front.wait(None).unwrap();
        evidence.record_command("SIGTERM 排空期间前端完成收尾", &front_done);
        let exited = world.wait_daemon(Some(wait(&budget))).unwrap();
        evidence.record_command("SIGTERM 排空完成后 lattice-daemon 退出", &exited);
        assert_eq!(
            exited.exit_code(),
            Some(0),
            "正常排空完成后台应以 0 退出：{}",
            exited.summary()
        );
        assert!(
            !exited.cleanup.forced && exited.cleanup.succeeded(),
            "{:?}",
            exited.cleanup
        );
        world.daemon_departed().unwrap();

        world
            .start_daemon()
            .expect("排空退出后显式重启后台以读取持久结果");
        let completed = detail(&world, &budget, &mut evidence, &tid);
        assert_eq!(
            completed["summary"]["outcome"], "completed",
            "排空完成后该轮持久结局应为 `completed`：{completed}"
        );
        archive_state(&world, &budget, &mut evidence, &[&tid]);
        evidence
            .record_json("parameters", &json!({"session": sid, "turn": tid}))
            .unwrap();
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn sigint_right_after_ready_exits_zero_and_releases_resources() {
    let (world, spec) = direct_world("background-sigint-ready", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 就绪后立即发信号，中间不插入提供方准备或其他 IPC 请求。
        world.start_daemon().expect("显式启动后台");
        world.interrupt_daemon().expect("向自有后台发送 `SIGINT`");
        let exited = world.wait_daemon(Some(wait(&budget))).unwrap();
        evidence.record_command("收到 SIGINT 后 lattice-daemon 正常退出", &exited);
        assert_eq!(
            exited.exit_code(),
            Some(0),
            "无在途工作时 `SIGINT` 停机应以 0 退出：{}",
            exited.summary()
        );
        assert!(
            !exited.cleanup.forced && exited.cleanup.succeeded(),
            "{:?}",
            exited.cleanup
        );
        world.daemon_departed().unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn sigterm_drain_timeout_exits_four_and_leaves_vanished_owner_turn() {
    let (world, spec) = direct_world("background-sigterm-drain-timeout", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world
            .start_daemon_with(&DaemonOptions::new().drain_timeout("300ms"))
            .expect("显式启动带排空超时的后台");
        let (server, gate) = wire_gated_fake_llm(&world, "background", SseScript::plain("next"));
        let front = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                "vanish on drain timeout",
                "--agent",
                "background",
            ],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();
        let (sid, tid) = running_turns(&world, &budget, &mut evidence)[0].clone();

        let stop_started = Instant::now();
        world.terminate_daemon().expect("向自有后台发送 `SIGTERM`");
        let exited = world
            .wait_daemon(Some(Duration::from_millis(20_300)))
            .unwrap();
        let stop_elapsed = stop_started.elapsed();
        evidence.record_command("排空超时后 lattice-daemon 以状态码 4 退出", &exited);
        assert!(
            stop_elapsed <= Duration::from_millis(20_300),
            "排空超时停机应在 20.3 秒内退出，实际耗时 {:?}",
            stop_elapsed
        );
        assert_eq!(
            exited.exit_code(),
            Some(4),
            "排空超时后台应以 4 退出：{}",
            exited.summary()
        );
        assert!(
            !exited.cleanup.forced && exited.cleanup.succeeded(),
            "{:?}",
            exited.cleanup
        );
        world.daemon_departed().unwrap();

        let lost = front.wait(None).unwrap();
        evidence.record_command("排空超时后前端检测到后台断开", &lost);
        assert!(lost.is_clean_exit(1), "{}", lost.summary());

        world.start_daemon().expect("排空超时退出后显式启动新后台");
        let stopped = detail(&world, &budget, &mut evidence, &tid);
        assert_ne!(stopped["summary"]["progress"]["progress"], "closed");
        let mut watcher = Watcher::open(&world, wait(&budget), &sid);
        let head = watcher.initial().head.clone().unwrap();
        assert_eq!(head.turn_id.as_deref(), Some(tid.as_str()));
        assert!(
            !head.owner,
            "重启后台后会话头 `owner` 应为 `false`：{head:?}"
        );
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

        gate.release(wait(&budget)).unwrap();
        let next = run(
            &world,
            &budget,
            &mut evidence,
            &["--json", "run", "after drain timeout", "--session", &sid],
        );
        assert!(next.is_clean_exit(0), "{}", next.summary());
        assert_eq!(detail(&world, &budget, &mut evidence, &tid), stopped);
        assert_eq!(server.captured_requests().len(), 2);
        archive_state(&world, &budget, &mut evidence, &[&tid]);
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn ipc_daemon_stop_drain_timeout_reports_unconfirmed_and_daemon_exits_four() {
    let (world, spec) = direct_world("background-ipc-stop-timeout", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world
            .start_daemon_with(&DaemonOptions::new().drain_timeout("300ms"))
            .expect("显式启动带排空超时的后台");
        let (server, gate) = wire_gated_fake_llm(&world, "background", SseScript::plain("stalled"));
        let front = command(
            &world,
            &budget,
            &["--json", "run", "ipc stop stall", "--agent", "background"],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();

        let stop_started = Instant::now();
        let stopped = driver()
            .run(
                &world,
                &LatticeCommand {
                    args: vec!["--json".into(), "daemon".into(), "stop".into()],
                    timeout: Some(Duration::from_millis(20_300)),
                },
            )
            .unwrap();
        let stop_elapsed = stop_started.elapsed();
        evidence.record_command("排空超时的 daemon stop 命令", &stopped);
        assert!(
            stop_elapsed <= Duration::from_millis(20_300),
            "`daemon stop` 应在 20.3 秒内返回，实际耗时 {:?}",
            stop_elapsed
        );
        world.daemon_departed().unwrap();
        assert!(
            stopped.is_clean_exit(1),
            "排空超时时 `daemon stop` 应以 1 退出：{}",
            stopped.summary()
        );
        let status: Value = serde_json::from_slice(&stopped.stdout).unwrap();
        assert!(
            status["unconfirmed_operations"].as_u64().unwrap_or(0) >= 1,
            "`unconfirmed_operations` 应不小于 1：{status}"
        );
        let stderr = String::from_utf8_lossy(&stopped.stderr);
        assert!(
            stderr.contains("没有确认收尾") && stderr.contains("执行者消失"),
            "`daemon stop` 标准错误应含提示，实际：{stderr:?}"
        );

        let daemon_out = world.wait_daemon(Some(wait(&budget))).unwrap();
        evidence.record_command("IPC 停机排空超时后 lattice-daemon 退出", &daemon_out);
        assert!(
            daemon_out.is_clean_exit(4),
            "后台应以 4 退出：{}",
            daemon_out.summary()
        );
        let lost = front.wait(None).unwrap();
        evidence.record_command("IPC 停机排空超时后前端退出", &lost);
        assert!(lost.is_clean_exit(1), "{}", lost.summary());
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn second_sigterm_during_drain_triggers_immediate_timeout_exit_four() {
    let (world, spec) = direct_world("background-second-sigterm", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world
            .start_daemon_with(&DaemonOptions::new().drain_timeout("60s"))
            .expect("显式启动长排空期后台");
        let (server, gate) = wire_gated_fake_llm(&world, "background", SseScript::plain("stalled"));
        let front = command(
            &world,
            &budget,
            &[
                "--json",
                "run",
                "second signal stall",
                "--agent",
                "background",
            ],
        );
        gate.wait_until_stalled(1, wait(&budget)).unwrap();

        world
            .terminate_daemon()
            .expect("第一次向自有后台发送 `SIGTERM`");
        poll_daemon_status(
            &world,
            &budget,
            &mut evidence,
            |status| status["stopping"] == true,
            "第一次 `SIGTERM` 后后台未进入 `stopping`",
        );
        assert!(
            !world.daemon_has_exited().unwrap(),
            "60 秒排空期内后台在第二次信号前不应退出"
        );

        let second_started = Instant::now();
        world
            .terminate_daemon()
            .expect("第二次向自有后台发送 `SIGTERM`");
        let daemon_out = world.wait_daemon(Some(Duration::from_secs(20))).unwrap();
        let elapsed = second_started.elapsed();
        evidence.record_command("第二次收到 SIGTERM 后 lattice-daemon 立即退出", &daemon_out);
        assert!(
            elapsed <= Duration::from_secs(20),
            "第二次 `SIGTERM` 起应在 20 秒内退出，实际耗时 {:?}",
            elapsed
        );
        assert_eq!(
            daemon_out.exit_code(),
            Some(4),
            "第二次信号触发排空超时应以 4 退出：{}",
            daemon_out.summary()
        );
        assert!(
            !daemon_out.cleanup.forced && daemon_out.cleanup.succeeded(),
            "{:?}",
            daemon_out.cleanup
        );
        world.daemon_departed().unwrap();

        let lost = front.wait(None).unwrap();
        evidence.record_command("第二次 SIGTERM 后前端检测到后台断开", &lost);
        assert!(lost.is_clean_exit(1), "{}", lost.summary());
        evidence.record_captures(server.captured_requests());
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn assertion_panic_unwinds_and_cleans_up_owned_daemon() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    fn open_pidfd(pid: u32) -> OwnedFd {
        // SAFETY: `pidfd_open` 不接收指针；成功时返回新文件描述符。
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(
            fd >= 0,
            "打开后台成员 pid={pid} 的 pidfd 失败：{}",
            std::io::Error::last_os_error()
        );
        // SAFETY: 系统调用成功，返回的文件描述符由 `OwnedFd` 独占管理。
        unsafe { OwnedFd::from_raw_fd(fd as i32) }
    }

    fn pidfd_exited(fd: &OwnedFd) -> bool {
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` 指向有效栈结构，仅非阻塞查询该内核进程实例是否已退出。
        let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
        assert!(
            rc >= 0,
            "轮询 pidfd 失败：{}",
            std::io::Error::last_os_error()
        );
        rc > 0 && (pfd.revents & libc::POLLIN != 0)
    }

    let mut recorded_pgid = None;
    let mut recorded_members: Vec<(u32, OwnedFd)> = Vec::new();
    let mut recorded_state_root = None;

    let unwind_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (world, spec) = direct_world("background-assertion-panic-cleanup", "background", "");
        let _budget = world.budget(&spec);
        world.start_daemon().expect("显式启动后台");

        let pgid = world.daemon_pgid().expect("自有后台进程组号");
        let pids = lattice_system_harness::processes_in_pgid(pgid).expect("读取自有后台进程组成员");
        assert!(
            !pids.is_empty() && pids.contains(&pgid),
            "断言前自有后台进程组应包含组首领：pgid={pgid} pids={pids:?}"
        );
        let state_root = world
            .session_store_dir()
            .parent()
            .expect("场景状态根")
            .to_path_buf();

        recorded_pgid = Some(pgid);
        recorded_members = pids.into_iter().map(|pid| (pid, open_pidfd(pid))).collect();
        recorded_state_root = Some(state_root);

        assert_eq!("实际值", "预期值", "受控触发的场景断言 panic");
    }));

    let panic_payload = unwind_result.expect_err("场景闭包应触发断言 panic");
    let panic_message = panic_payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            panic_payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
        })
        .unwrap_or_default();
    assert!(
        panic_message.contains("受控触发的场景断言 panic"),
        "展开后应保留原始断言消息，实际：{panic_message:?}"
    );

    let pgid = recorded_pgid.expect("已记录进程组号");
    assert!(
        !recorded_members.is_empty(),
        "应已记录至少一个后台进程组成员"
    );
    assert!(
        lattice_system_harness::processes_in_pgid(pgid)
            .expect("展开后查询自有后台进程组")
            .is_empty(),
        "断言 panic 展开并丢弃 ScenarioWorld 后，后台进程组 pgid={pgid} 必须已退空"
    );
    for (pid, fd) in &recorded_members {
        assert!(
            pidfd_exited(fd),
            "断言 panic 展开并丢弃 ScenarioWorld 后，原后台组成员 pid={pid} 必须已退出"
        );
    }
    let state_root = recorded_state_root.expect("已记录状态根");
    assert!(
        !state_root.exists(),
        "清理成功完成后，临时场景状态根 {} 应已一并移除",
        state_root.display()
    );
}

#[test]
fn frontends_without_daemon_report_not_running_without_spawning_or_locking() {
    use lattice_system_harness::{PtyTuiDriver, assert_no_secret};

    const AUTH_SENTINEL: &str = "sk-not-running-auth-sentinel-9182";
    let (world, spec) = direct_world("background-frontends-not-running", "background", "");
    let budget = world.budget(&spec);
    let mut evidence =
        ScenarioEvidence::new(&world, &spec.name, [AUTH_SENTINEL.into()]).expect("创建场景证据");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let state_root = world
            .session_store_dir()
            .parent()
            .expect("场景状态根")
            .to_path_buf();
        let socket_path = state_root.join("lattice/local/api.sock");
        let owner_lock_path = state_root.join("lattice/local/owner.lock");

        type JsonCase<'a> = (&'a str, Vec<&'a str>, Option<Vec<u8>>);
        let json_cases: Vec<JsonCase<'_>> = vec![
            (
                "session send（JSON）",
                vec![
                    "--json",
                    "session",
                    "send",
                    "hello",
                    "--agent",
                    "background",
                ],
                None,
            ),
            (
                "run（JSON）",
                vec!["--json", "run", "hello", "--agent", "background"],
                None,
            ),
            (
                "session list（JSON）",
                vec!["--json", "session", "list"],
                None,
            ),
            ("models 列表（JSON）", vec!["--json", "models"], None),
            (
                "models refresh（JSON）",
                vec!["--json", "models", "refresh"],
                None,
            ),
            (
                "auth status（JSON）",
                vec!["--json", "auth", "status"],
                None,
            ),
            (
                "auth set（JSON）",
                vec!["--json", "auth", "set", "openai"],
                Some(format!("{AUTH_SENTINEL}\n").into_bytes()),
            ),
            (
                "auth remove（JSON）",
                vec!["--json", "auth", "remove", "openai"],
                None,
            ),
            (
                "auth logout（JSON）",
                vec!["--json", "auth", "logout", "chatgpt"],
                None,
            ),
            (
                "auth login（JSON）",
                vec!["--json", "auth", "login", "chatgpt"],
                None,
            ),
            (
                "tui（JSON）",
                vec!["--json", "tui", "--agent", "background"],
                None,
            ),
        ];

        for (label, args, stdin_bytes) in json_cases {
            let cmd = LatticeCommand {
                args: args.iter().map(|arg| (*arg).into()).collect(),
                timeout: Some(wait(&budget)),
            };
            let out = match stdin_bytes {
                Some(ref input) => driver().run_with_stdin(&world, &cmd, input).unwrap(),
                None => driver().run(&world, &cmd).unwrap(),
            };
            evidence.record_command(format!("无后台调用前端（{label}）"), &out);
            assert_no_secret(&out, AUTH_SENTINEL).unwrap();
            assert!(
                out.is_clean_exit(1),
                "{label} 在无后台时必须干净地以退出码 1 退出：{}",
                out.summary()
            );
            let parsed_lines = lines(&out);
            assert_eq!(
                parsed_lines.len(),
                1,
                "{label} 在无后台时只能输出单条 `not_running` 错误记录，不应发出 `sending` 或回执：{parsed_lines:?}"
            );
            assert_eq!(
                parsed_lines[0]["type"], "error",
                "{label}: {parsed_lines:?}"
            );
            assert_eq!(
                parsed_lines[0]["kind"], "not_running",
                "{label}: {parsed_lines:?}"
            );
            let msg = parsed_lines[0]["message"].as_str().unwrap_or("");
            for required in [
                "后台未运行",
                "systemctl --user start lattice",
                "lattice daemon install",
            ] {
                assert!(
                    msg.contains(required),
                    "{label} 的中文提示缺少 `{required}`：{msg}"
                );
            }
        }

        for (label, args) in [
            ("run（文本）", vec!["run", "hello", "--agent", "background"]),
            ("models（文本）", vec!["models"]),
            ("auth status（文本）", vec!["auth", "status"]),
        ] {
            let out = run(&world, &budget, &mut evidence, &args);
            assert!(
                out.is_clean_exit(1),
                "{label} 在无后台时必须以退出码 1 退出：{}",
                out.summary()
            );
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains("后台未运行"),
                "{label} 标准错误应包含中文未运行提示：{stderr}"
            );
        }

        let mut tui = PtyTuiDriver::spawn(&driver(), &world, "background")
            .expect("在无后台环境启动伪终端 TUI");
        let tui_code = tui
            .wait_for_exit_code(wait(&budget))
            .expect("无后台时伪终端 TUI 应立即自然退出");
        let tui_output = String::from_utf8_lossy(tui.output()).into_owned();
        evidence
            .record_json(
                "pty-tui-not-running",
                &json!({"exit_code": tui_code, "output": tui_output}),
            )
            .unwrap();
        assert_eq!(tui_code, 1, "无后台时伪终端 TUI 必须以退出码 1 退出");
        assert!(
            tui_output.contains("后台未运行"),
            "伪终端 TUI 应输出中文 `not_running` 提示：{tui_output:?}"
        );

        assert!(
            !socket_path.exists(),
            "无后台调用前端后状态根下不得出现 `api.sock`：{}",
            socket_path.display()
        );
        assert!(
            !owner_lock_path.exists(),
            "无后台调用前端后状态根下不得创建 `owner.lock`：{}",
            owner_lock_path.display()
        );
        assert!(
            world.daemon_pids().unwrap().is_empty(),
            "前端调用不得自动拉起任何后台进程"
        );
        world
            .daemon_departed()
            .expect("无后台场景结束时不应存在任何套接字、实例锁持有者或残留 `lattice-daemon` 进程");
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn external_holder_of_owner_lock_causes_lattice_daemon_to_exit_three() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    let (world, spec) = direct_world("background-owner-lock-held", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let state_root = world
            .session_store_dir()
            .parent()
            .expect("场景状态根")
            .to_path_buf();
        let local_dir = state_root.join("lattice/local");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&local_dir)
            .expect("以 0700 权限创建状态根 local 目录");
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(local_dir.join("owner.lock"))
            .expect("打开测试占用的 owner.lock");
        struct FlockGuard(std::fs::File);
        impl Drop for FlockGuard {
            fn drop(&mut self) {
                // `flock` 属于打开文件描述；并行测试线程在 `fork` 之后、`exec` 之前会短暂持有副本（#228），显式 `LOCK_UN` 立即解锁整个描述。
                // SAFETY: `self.0` 在 Drop 期间持有有效文件描述符。
                unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
            }
        }
        // SAFETY: `lock_file` 为当前测试进程独占打开的有效文件描述符；非阻塞加排他锁并交由 `FlockGuard` 管理。
        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(
            rc,
            0,
            "测试自身获取 owner.lock 失败：{}",
            std::io::Error::last_os_error()
        );
        let lock_guard = FlockGuard(lock_file);

        let out = support::daemon_driver()
            .run(
                &world,
                &LatticeCommand {
                    args: vec!["--state-root".into(), state_root.display().to_string()],
                    timeout: Some(wait(&budget)),
                },
            )
            .expect("启动争抢已占用状态根的 lattice-daemon");
        evidence.record_command("owner.lock 已被外部占用时启动 lattice-daemon", &out);
        assert!(
            out.is_clean_exit(3),
            "取不到 `owner.lock` 时 `lattice-daemon` 必须干净地以退出码 3 退出：{}",
            out.summary()
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("状态根已被另一个后台占用（对端进程号不可用）"),
            "无监听套接字时应输出中文对端进程号不可用诊断：{stderr}"
        );

        drop(lock_guard);
        world
            .daemon_departed()
            .expect("释放测试锁后状态根应保持干净");
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn notify_socket_delivers_ready_after_bind_and_stopping_across_shutdown_paths() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram, UnixStream};

    enum ShutdownTrigger {
        IpcStop,
        Sigterm,
        Sigint,
    }

    let (world, spec) = direct_world("background-notify-socket", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let state_root = world
            .session_store_dir()
            .parent()
            .expect("场景状态根")
            .to_path_buf();
        let api_sock = state_root.join("lattice/local/api.sock");
        let daemon_drv = support::daemon_driver();

        let notify_dir = tempfile::Builder::new()
            .prefix("ltc-ntf-")
            .tempdir_in("/tmp")
            .expect("创建短路径通知套接字目录");

        let cases = [
            (
                "文件路径 NOTIFY_SOCKET + IPC stop",
                false,
                ShutdownTrigger::IpcStop,
            ),
            (
                "抽象地址 NOTIFY_SOCKET + SIGTERM",
                true,
                ShutdownTrigger::Sigterm,
            ),
            (
                "文件路径 NOTIFY_SOCKET + SIGINT",
                false,
                ShutdownTrigger::Sigint,
            ),
        ];

        for (idx, (label, use_abstract, trigger)) in cases.into_iter().enumerate() {
            let (receiver, env_val) = if use_abstract {
                let name = format!("lattice-notify-{}-{idx}", std::process::id());
                let addr = SocketAddr::from_abstract_name(name.as_bytes())
                    .expect("构造抽象命名空间套接字地址");
                let sock = UnixDatagram::bind_addr(&addr).expect("绑定抽象通知数据报套接字");
                (sock, format!("@{name}"))
            } else {
                let sock_path = notify_dir.path().join(format!("notify-{idx}.sock"));
                let sock = UnixDatagram::bind(&sock_path).expect("绑定文件通知数据报套接字");
                (sock, sock_path.display().to_string())
            };
            receiver
                .set_read_timeout(Some(wait(&budget)))
                .expect("设置通知读取超时");

            world.set_env("NOTIFY_SOCKET", &env_val);
            let mut proc = daemon_drv
                .start(
                    &world,
                    &LatticeCommand {
                        args: vec!["--state-root".into(), state_root.display().to_string()],
                        timeout: Some(wait(&budget)),
                    },
                )
                .expect("启动带 NOTIFY_SOCKET 的 lattice-daemon");

            let mut buf = [0u8; 64];
            let ready_len = receiver
                .recv(&mut buf)
                .unwrap_or_else(|err| panic!("{label} 未在期限内收到 READY=1：{err}"));
            assert_eq!(
                &buf[..ready_len],
                b"READY=1",
                "{label} 首条通知报文必须为 `READY=1`"
            );

            // 收到 `READY=1` 的同一刻不作重试直接连接 `api.sock`：若 `READY=1` 在绑定发布前发出，此处会立即失败。
            let immediate_conn = UnixStream::connect(&api_sock).unwrap_or_else(|err| {
                panic!(
                    "{label} 收到 `READY=1` 时监听套接字 `{}` 必须已就绪可连，但直接连接失败：{err}",
                    api_sock.display()
                )
            });
            drop(immediate_conn);

            receiver.set_nonblocking(true).expect("切换非阻塞检查");
            match receiver.recv(&mut buf) {
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Ok(n) => panic!(
                    "{label} 停机触发前不应提前收到通知报文：{:?}",
                    String::from_utf8_lossy(&buf[..n])
                ),
                Err(err) => panic!("{label} 非阻塞检查通知套接字失败：{err}"),
            }
            receiver.set_nonblocking(false).expect("恢复阻塞等待");
            receiver
                .set_read_timeout(Some(wait(&budget)))
                .expect("恢复通知读取超时");

            match trigger {
                ShutdownTrigger::IpcStop => {
                    let stopped = run(
                        &world,
                        &budget,
                        &mut evidence,
                        &["--json", "daemon", "stop"],
                    );
                    assert!(stopped.is_clean_exit(0), "{label}: {}", stopped.summary());
                }
                ShutdownTrigger::Sigterm => {
                    proc.terminate().expect("向 lattice-daemon 发送 SIGTERM");
                }
                ShutdownTrigger::Sigint => {
                    proc.interrupt().expect("向 lattice-daemon 发送 SIGINT");
                }
            }

            let stopping_len = receiver
                .recv(&mut buf)
                .unwrap_or_else(|err| panic!("{label} 停机开始时未收到 STOPPING=1：{err}"));
            assert_eq!(
                &buf[..stopping_len],
                b"STOPPING=1",
                "{label} 停机通知报文必须为 `STOPPING=1`"
            );

            let out = proc
                .wait(Some(wait(&budget)))
                .expect("等待 lattice-daemon 退出");
            evidence.record_command(format!("通知场景后台退出（{label}）"), &out);
            assert_eq!(
                out.exit_code(),
                Some(0),
                "{label} 正常停机应以退出码 0 退出：{}",
                out.summary()
            );
            assert!(
                out.stdout_complete
                    && out.stderr_complete
                    && !out.cleanup.forced
                    && out.cleanup.succeeded(),
                "{label} 后台退出清理异常：{}",
                out.summary()
            );
            world.unset_env("NOTIFY_SOCKET");
            world
                .daemon_departed()
                .expect("单轮通知场景结束后状态根应干净");
        }

        // 未设置 `NOTIFY_SOCKET` 时，`lattice-daemon` 照常启动、提供状态查询并正常停机。
        world.unset_env("NOTIFY_SOCKET");
        world
            .start_daemon()
            .expect("未设置 NOTIFY_SOCKET 时正常启动后台");
        let status = read(
            &world,
            &budget,
            &mut evidence,
            &["--json", "daemon", "status"],
        );
        assert_eq!(status["stopping"], false);
        world
            .stop_daemon()
            .expect("未设置 NOTIFY_SOCKET 时正常停止后台");
        world
            .daemon_departed()
            .expect("未设置 NOTIFY_SOCKET 场景结束后状态根应干净");
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn daemon_startup_and_shutdown_failures_report_chinese_diagnostics_and_exit_one() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let (world, spec) = direct_world("background-daemon-chinese-errors", "background", "");
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let daemon_drv = support::daemon_driver();

        // 1. `--state-root` 指向普通文件：退出 1，中文指明路径与非目录原因，不含英文连接失败提示。
        let plain_dir = tempfile::Builder::new()
            .prefix("ltc-file-")
            .tempdir_in("/tmp")
            .expect("创建普通文件探针目录");
        let plain_file = plain_dir.path().join("state-root-plain-file");
        std::fs::write(&plain_file, b"plain file").expect("写入普通文件探针");
        let file_out = daemon_drv
            .run(
                &world,
                &LatticeCommand {
                    args: vec!["--state-root".into(), plain_file.display().to_string()],
                    timeout: Some(wait(&budget)),
                },
            )
            .expect("以普通文件作为 --state-root 运行 lattice-daemon");
        evidence.record_command("--state-root 指向普通文件时启动 lattice-daemon", &file_out);
        std::fs::remove_file(&plain_file).expect("清理普通文件探针");
        assert!(
            file_out.is_clean_exit(1),
            "`--state-root` 指向普通文件时必须干净地以退出码 1 退出：{}",
            file_out.summary()
        );
        let file_stderr = String::from_utf8_lossy(&file_out.stderr);
        assert!(
            file_stderr.contains("错误：")
                && file_stderr.contains(&plain_file.display().to_string())
                && file_stderr.contains("目录")
                && !file_stderr.contains("local connection failed; acknowledgement may be unknown")
                && !file_stderr.contains("local endpoint ownership or permissions rejected")
                && !file_stderr.contains("后台未运行"),
            "`--state-root` 指向普通文件应输出含路径的中文说明，实际：{file_stderr:?}"
        );

        // 2. `lattice/local` 目录权限设为 0777：退出 1，中文说明权限被拒，不含旧英文原文。
        let perm_root_dir = tempfile::Builder::new()
            .prefix("ltc-perm-")
            .tempdir_in("/tmp")
            .expect("创建权限探针隔离状态根");
        let perm_local = perm_root_dir.path().join("lattice/local");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&perm_local)
            .expect("创建 lattice/local 目录");
        std::fs::set_permissions(&perm_local, std::fs::Permissions::from_mode(0o777))
            .expect("将 lattice/local 权限设为 0777");
        let perm_out = daemon_drv
            .run(
                &world,
                &LatticeCommand {
                    args: vec![
                        "--state-root".into(),
                        perm_root_dir.path().display().to_string(),
                    ],
                    timeout: Some(wait(&budget)),
                },
            )
            .expect("在 0777 local 目录下运行 lattice-daemon");
        evidence.record_command("lattice/local 权限为 0777 时启动 lattice-daemon", &perm_out);
        assert!(
            perm_out.is_clean_exit(1),
            "`lattice/local` 权限为 0777 时必须干净地以退出码 1 退出：{}",
            perm_out.summary()
        );
        let perm_stderr = String::from_utf8_lossy(&perm_out.stderr);
        assert!(
            perm_stderr.contains("错误：")
                && perm_stderr.contains("权限")
                && perm_stderr.contains("0777")
                && !perm_stderr.contains("local endpoint ownership or permissions rejected")
                && !perm_stderr.contains("local connection failed")
                && !perm_stderr.contains("后台未运行"),
            "0777 目录权限探针应输出中文权限报错，实际：{perm_stderr:?}"
        );

        // 3. 后台就绪后删掉 `api.sock`，再发 SIGTERM：以 1 退出，输出说明 socket 清理失败，不含“后台未运行”。
        let state_root = world
            .session_store_dir()
            .parent()
            .expect("场景状态根")
            .to_path_buf();
        let api_sock = state_root.join("lattice/local/api.sock");
        let mut proc = daemon_drv
            .start(
                &world,
                &LatticeCommand {
                    args: vec!["--state-root".into(), state_root.display().to_string()],
                    timeout: Some(wait(&budget)),
                },
            )
            .expect("启动待测试 socket 清理失败的 lattice-daemon");

        let ready_deadline = std::time::Instant::now() + wait(&budget);
        loop {
            let probe = driver()
                .run(
                    &world,
                    &LatticeCommand {
                        args: vec!["--json".into(), "daemon".into(), "status".into()],
                        timeout: Some(Duration::from_secs(2)),
                    },
                )
                .expect("轮询 daemon status");
            if probe.is_clean_exit(0) {
                break;
            }
            assert!(
                std::time::Instant::now() < ready_deadline,
                "lattice-daemon 未在期限内就绪：{}",
                probe.summary()
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        std::fs::remove_file(&api_sock).expect("就绪后删除 api.sock");
        proc.terminate().expect("删除 api.sock 后发送 SIGTERM");
        let cleanup_out = proc
            .wait(Some(wait(&budget)))
            .expect("等待 lattice-daemon 在 SIGTERM 后退出");
        evidence.record_command(
            "删除 api.sock 后收到 SIGTERM 退出的 lattice-daemon",
            &cleanup_out,
        );
        assert_eq!(
            cleanup_out.exit_code(),
            Some(1),
            "删除 `api.sock` 后收到 SIGTERM 应以退出码 1 退出：{}",
            cleanup_out.summary()
        );
        assert!(
            cleanup_out.stdout_complete
                && cleanup_out.stderr_complete
                && !cleanup_out.cleanup.forced
                && cleanup_out.cleanup.succeeded(),
            "删除 `api.sock` 场景后台退出清理异常：{}",
            cleanup_out.summary()
        );
        let cleanup_stderr = String::from_utf8_lossy(&cleanup_out.stderr);
        assert!(
            cleanup_stderr.contains("错误：")
                && cleanup_stderr.contains("清理 socket 失败")
                && cleanup_stderr.contains("api.sock")
                && !cleanup_stderr.contains("后台未运行")
                && !cleanup_stderr.contains("systemctl --user start lattice")
                && !cleanup_stderr.contains("lattice daemon install"),
            "删除 `api.sock` 后停机应报告 socket 清理失败且不得提示后台未运行，实际：{cleanup_stderr:?}"
        );
        world
            .daemon_departed()
            .expect("异常清理退出后实例锁与进程组仍应完全释放");
    }));
    support::finish_recorded(evidence, result);
}
