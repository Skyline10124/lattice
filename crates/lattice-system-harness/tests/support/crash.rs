// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_system_harness::{RunningCommand, ScenarioEvidence, ScenarioSpec, ScenarioWorld};
use std::time::{Duration, Instant};

/// 屏障环境在后台启动时固定；复用统一生命周期停止旧后台、按当前环境快照重启并等待就绪。
pub(crate) fn start(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    budget: &lattice_system_harness::ScenarioBudget,
) -> u32 {
    budget.check(&spec.name);
    world.restart_daemon().expect("启动特性后台");
    let pgid = world.daemon_pgid().expect("特性后台进程组标识");
    assert_eq!(world.daemon_pids().unwrap(), vec![pgid]);
    pgid
}

/// 先核验后台确实停在 `SIGSTOP` 窗口，再强制终止后台；前端只能因连接关闭自行退出。
pub(crate) fn kill(
    world: &ScenarioWorld,
    daemon_pgid: u32,
    frontend: RunningCommand,
    evidence: &mut ScenarioEvidence,
    budget: &lattice_system_harness::ScenarioBudget,
) {
    evidence
        .record_json(
            "crash-processes",
            &serde_json::json!({
                "backend_pid": daemon_pgid,
                "backend_pgid": daemon_pgid,
                "frontend_pgid": frontend.pgid(),
                "binary": super::driver().binary_path(),
                "state_root": world.session_store_dir().parent(),
            }),
        )
        .unwrap();
    assert_ne!(daemon_pgid, frontend.pgid());
    kill_backend(world, daemon_pgid, evidence, budget);
    let disconnected = frontend.wait(None).unwrap();
    evidence.record_command("frontend observes backend crash", &disconnected);
    assert!(disconnected.is_clean_exit(1), "{}", disconnected.summary());
}

/// 只强制终止停在屏障的自有后台，前端的调度与等待由场景控制。
pub(crate) fn kill_backend(
    world: &ScenarioWorld,
    daemon_pgid: u32,
    evidence: &mut ScenarioEvidence,
    budget: &lattice_system_harness::ScenarioBudget,
) {
    assert_eq!(world.daemon_pids().unwrap(), vec![daemon_pgid]);
    let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(2));
    loop {
        assert!(
            Instant::now() < deadline,
            "后台进入 `SIGSTOP` 前场景预算已耗尽"
        );
        let stat = std::fs::read_to_string(format!("/proc/{daemon_pgid}/stat")).unwrap();
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
            "屏障确认文件写入后后台未进入 `SIGSTOP`"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let killed = world.kill_daemon().expect("强制终止停在屏障的自有后台");
    evidence.record_command("SIGKILL acknowledged backend barrier", &killed);
    super::assert_forced_kill(&killed);
    assert!(world.daemon_pids().unwrap().is_empty());
}
