// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_system_harness::{
    LatticeCommand, RunningCommand, ScenarioEvidence, ScenarioSpec, ScenarioWorld,
};
use std::time::{Duration, Instant};

/// 屏障环境在后台启动时固定；不得通过重发前端请求改变常驻进程的环境。
pub(crate) fn start(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    budget: &lattice_system_harness::ScenarioBudget,
) -> RunningCommand {
    budget.check(&spec.name);
    let owners = world.daemon_pids().expect("inspect previous daemon");
    if !owners.is_empty() {
        let stopped = super::driver()
            .run(
                world,
                &LatticeCommand {
                    args: vec!["--json".into(), "daemon".into(), "stop".into()],
                    timeout: Some(budget.cap_timeout(spec.step_timeout())),
                },
            )
            .expect("stop previous scenario daemon");
        assert!(stopped.is_clean_exit(0), "{}", stopped.summary());
        let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(2));
        while owners.iter().any(|pid| {
            !lattice_system_harness::processes_in_pgid(*pid)
                .unwrap()
                .is_empty()
        }) {
            assert!(
                Instant::now() < deadline,
                "previous daemon did not drain within scenario budget"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    budget.check(&spec.name);
    let daemon = super::driver()
        .start(
            world,
            &LatticeCommand {
                args: vec![
                    "daemon".into(),
                    "serve".into(),
                    "--state-root".into(),
                    world
                        .session_store_dir()
                        .parent()
                        .unwrap()
                        .display()
                        .to_string(),
                ],
                timeout: Some(budget.cap_timeout(spec.step_timeout())),
            },
        )
        .expect("start feature daemon");
    let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(5));
    loop {
        budget.check(&spec.name);
        let status = super::driver()
            .run(
                world,
                &LatticeCommand {
                    args: vec!["--json".into(), "daemon".into(), "status".into()],
                    timeout: Some(
                        budget
                            .cap_timeout(spec.step_timeout())
                            .min(deadline.saturating_duration_since(Instant::now())),
                    ),
                },
            )
            .expect("query feature daemon");
        if status.is_clean_exit(0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "feature daemon did not start: {}",
            status.summary()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(world.daemon_pids().unwrap(), vec![daemon.pgid()]);
    daemon
}

/// 先核验后台确实停在SIGSTOP窗口，再杀后台；前端只能因连接关闭自行退出。
pub(crate) fn kill(
    world: &ScenarioWorld,
    daemon: RunningCommand,
    frontend: RunningCommand,
    evidence: &mut ScenarioEvidence,
    budget: &lattice_system_harness::ScenarioBudget,
) {
    evidence.record_json("crash-processes", &serde_json::json!({
        "backend_pid":daemon.pgid(),"backend_pgid":daemon.pgid(),"frontend_pgid":frontend.pgid(),
        "binary":super::driver().binary_path(),"state_root":world.session_store_dir().parent()
    })).unwrap();
    assert_ne!(daemon.pgid(), frontend.pgid());
    kill_backend(world, daemon, evidence, budget);
    let disconnected = frontend.wait(None).unwrap();
    evidence.record_command("frontend observes backend crash", &disconnected);
    assert!(disconnected.is_clean_exit(1), "{}", disconnected.summary());
}

/// 只杀停在屏障的自有后台，前端的调度与等待由场景控制。
pub(crate) fn kill_backend(
    world: &ScenarioWorld,
    daemon: RunningCommand,
    evidence: &mut ScenarioEvidence,
    budget: &lattice_system_harness::ScenarioBudget,
) {
    assert_eq!(world.daemon_pids().unwrap(), vec![daemon.pgid()]);
    let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(2));
    loop {
        assert!(
            Instant::now() < deadline,
            "scenario budget expired before backend SIGSTOP"
        );
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", daemon.pgid())).unwrap();
        if stat
            .rsplit_once(')')
            .unwrap()
            .1
            .trim_start()
            .starts_with('T')
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "ack was not followed by backend SIGSTOP"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let killed = daemon.kill_and_wait().unwrap();
    evidence.record_command("SIGKILL acknowledged backend barrier", &killed);
    super::assert_forced_kill(&killed);
    assert!(world.daemon_pids().unwrap().is_empty());
}
