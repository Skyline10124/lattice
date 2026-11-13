// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 通过真实 PTY 验证 TUI 与后台的选择、执行、重连和取消接线。
//! 场景使用正式配置与假 TLS provider，核对请求 JSON 和公开会话事实。
//! 门控窗口和完整帧用于同步及检查可观察状态；颜色与可读性留给真实终端人工检查。

use std::time::Duration;

#[cfg(feature = "fake-tools")]
use lattice_system_harness::ScenarioSpec;
use lattice_system_harness::processes_in_pgid;
use lattice_system_harness::{
    LatticeDriver, ProviderFixture, PtyTuiDriver, ScenarioLayer, ScenarioWorld,
};

/// TUI 冒烟场景的项目配置；工具目录由 `LATTICE_TEST_TOOLS` 注入，调用顺序由 `two_step_script` 决定。
/// 手写 TOML 而不使用 `serde`，因为测试框架边界禁止依赖 `lattice-profile`。
#[cfg(feature = "fake-tools")]
fn agent_toml() -> String {
    let model = ProviderFixture::chat_completions("tui-smoke")
        .model_ref()
        .unwrap();
    format!(
        r#"schema_version = 1

[agent]
name = "tui-smoke"
model = "{model}"

[prompt]
system = "You are a concise test agent. Reply briefly."
"#
    )
}

/// 一个 Turn 两个 `Step`：先调用假工具 `record_note`，再输出三行最终回答。
#[cfg(feature = "fake-tools")]
fn two_step_script() -> lattice_system_harness::SseScript {
    use lattice_system_harness::SseScript;
    SseScript::Sequence {
        scripts: vec![
            SseScript::ToolCall {
                call_id: "smoke-call-1".into(),
                tool_name: "record_note".into(),
                arguments_json: r#"{"action":"read","value":{"note":"smoke"}}"#.into(),
            },
            SseScript::PlainText {
                chunks: (1..=3)
                    .map(|n| format!("mock line {n:02}: final answer.\n"))
                    .collect(),
                finish_reason: "stop".into(),
            },
            SseScript::plain("second copy reply"),
        ],
    }
}

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN 必须指向已构建的 lattice 二进制")
}

#[cfg(feature = "fake-tools")]
#[test]
fn pty_tui_smoke_runs_turn_and_quits_cleanly() {
    let driver = driver();

    let spec = ScenarioSpec {
        name: "pty-tui-smoke".into(),
        layer: ScenarioLayer::PtySmoke,
        step_timeout_secs: 120,
        total_timeout_secs: Some(300),
    };
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    world.set_env("LATTICE_TEST_TOOLS", "record_note");
    let budget = world.budget(&spec);
    world.init_git_project().expect("初始化 Git 项目失败");
    budget.check(&spec.name);
    world.start_daemon().expect("显式启动带测试工具的后台");

    let server = lattice_system_harness::FakeLlmServer::start(two_step_script())
        .expect("启动假模型服务失败");
    world
        .wire_provider(
            ProviderFixture::chat_completions("tui-smoke"),
            &server,
            lattice_system_harness::FAKE_API_KEY,
        )
        .unwrap();

    // `lattice tui --agent` 启动即经 `configure(New)` 建立草稿会话。
    world
        .write_project_file(".lattice/agents/tui-smoke/agent.toml", &agent_toml())
        .unwrap();
    let session =
        lattice_system_harness::SessionSendDriver::new(&driver, &world, &spec, "tui-smoke");

    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        budget.check(&spec.name);
        let mut tui = PtyTuiDriver::spawn(&driver, &world, "tui-smoke").expect("启动 TUI 失败");

        // 等待 TUI 进入备用屏幕。就绪识别封装在驱动层（`wait_for_ready`）内，
        // 场景自身不直接匹配终端控制字节（见 RFC #41 第三项决定）。
        tui.wait_for_ready(Duration::from_secs(30))
            .expect("TUI 未在时限内进入备用屏幕");
        // 会话与智能体只在状态行显示，启动横幅不重复；启动起就显示会话短标识。
        tui.wait_for(Duration::from_secs(30), |s| s.contains("显示命令"))
            .expect("未等到启动横幅出现");
        let listed = session
            .run_command(&["--json", "session", "list"])
            .expect("查询会话列表失败");
        assert!(listed.is_clean_exit(0), "{}", listed.summary());
        let list: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        let sid = list[0]["session_id"].as_str().unwrap();
        let start = tui
            .redraw_text(Duration::from_secs(10))
            .expect("读取启动完整帧失败");
        let status_prefix = format!("{} · tui-smoke · tui-smoke · local", &sid[..8]);
        assert!(
            start.matches(&sid[..8]).count() == 1
                && start.matches("tui-smoke").count() == 2
                && start.contains(&status_prefix)
                && !start.contains("未接受"),
            "会话与智能体信息必须仅出现在状态行中：{start}"
        );

        // 发送提示词：该轮次依次执行一个工具步骤与一个三行文本步骤。
        budget.check(&spec.name);
        tui.send_line("ping").expect("发送提示词失败");

        // Turn 关闭后，持久正文装入会替换实时块；实时事件积压被丢弃时，正文只经由这次装入到达。
        // 装入在屏幕上没有标志，整行移位后终端只重写变化的字符格，输出流里拼不出连续的文字，
        // 所以等它落定后取整帧判断。
        // 完成是 `Info` 级结局，直接进对话记录，不占停靠栏；随后的 `/quit` 仍能输入。
        tui.wait_for(Duration::from_secs(60), |s| s.contains("完成"))
            .expect("未等到轮次完成摘要出现");
        let frame = wait_pty_frame(
            &mut tui,
            budget.cap_timeout(Duration::from_secs(10)),
            |frame| frame.contains("mock line 03"),
        );
        // 假工具返回 `{"note":"smoke"}`（规范 JSON 共 16 字节），工具块在启动横幅与最终回答之间折叠显示。
        let banner = frame.find("显示命令");
        let tool_block = frame.find("▸ ✓ record_note · 16 B");
        let answer = frame.find("mock line 03");
        assert!(
            tool_block.is_some()
                && answer.is_some()
                && banner.is_none_or(|n| Some(n) < tool_block)
                && tool_block < answer,
            "工具块与最终回答必须按顺序位于启动横幅下方：{frame}"
        );
        // 两个 `Step` 同属一个 Turn：助手标题线只出现一次。状态行也显示智能体与模型名，不计入。
        assert_eq!(
            frame.matches("── tui-smoke ─").count(),
            1,
            "每个轮次只应显示一行助手标题线：{frame}"
        );
        // 首次提交在接受事务里才创建会话库；接受前不应有订阅，也就不应出现“库不可用”。
        let shown = String::from_utf8_lossy(tui.output()).into_owned();
        assert!(
            !shown.contains("store unavailable"),
            "首次提交不得报错提示缺少会话库"
        );

        let caps = server.captured_requests();
        assert!(
            caps.iter()
                .any(|c| c.model == "tui-smoke" && c.message_roles.iter().any(|r| r == "user")),
            "假模型服务未收到预期请求；已捕获：{caps:?}"
        );
        assert_eq!(
            support::tool_result_value(&caps[1]),
            Some(serde_json::json!({"note": "smoke"})),
            "假工具结果应投影进第二步模型请求：{caps:?}"
        );

        // 首次接受消费启动智能体；删去源配置后第二次提交仍继承副本。
        std::fs::remove_dir_all(world.project_dir().join(".lattice/agents/tui-smoke")).unwrap();
        tui.send_line("ping from copy").unwrap();
        tui.wait_for(Duration::from_secs(30), |screen| {
            screen.contains("second copy reply")
        })
        .unwrap();
        assert_eq!(server.captured_requests().len(), 3);
        for capture in server.captured_requests() {
            for field in [
                "reasoning_effort",
                "reasoning",
                "thinking",
                "enable_thinking",
            ] {
                assert!(capture.raw.get(field).is_none(), "未选择档位不发送 {field}");
            }
        }

        // `/quit` 必须让 TUI 自行退出。必须在 `Drop` 执行前观察子进程自行退出：
        // `Drop` 会强杀进程组，若只做残留检查，即使 `/quit` 完全无效也只会证明 `SIGKILL` 有效。
        tui.send_line("/quit").expect("发送 /quit 失败");
        tui.wait_for_exit(Duration::from_secs(15))
            .expect("/quit 必须在无外部强杀的情况下让 TUI 退出");

        evidence.record_pty(tui.output());
        // 后台运行在由 `ScenarioWorld` 持有的独立进程组中；此处仅要求前端进程组清空。
        // `Drop` 会强杀残留子进程从而掩盖孤儿进程，因此必须在交给 `Drop` 前完成检查。
        let pgid = tui.pgid().expect("TUI 应拥有前端进程组标识");
        let deadline = std::time::Instant::now() + budget.cap_timeout(Duration::from_secs(2));
        loop {
            let survivors = processes_in_pgid(pgid).expect("检查 TUI 进程组失败");
            if survivors.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "/quit 后、Drop 前 TUI 进程组 {pgid} 仍有存活成员：{survivors:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(tui);

        let session =
            lattice_system_harness::SessionSendDriver::new(&driver, &world, &spec, "tui-smoke");
        let out = session.run_command(&["--json", "session", "list"]).unwrap();
        evidence.record_command("查询会话列表（--json session list）", &out);
        assert!(out.is_clean_exit(0));
        let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let sid = list[0]["session_id"].as_str().unwrap();
        let log_out = session
            .run_command(&["--json", "session", "log", sid])
            .unwrap();
        evidence.record_command("检查首轮会话树（--json session log）", &log_out);
        assert!(log_out.is_clean_exit(0));
        let log: serde_json::Value = serde_json::from_slice(&log_out.stdout).unwrap();
        let first_tid = log["nodes"][1]["turn_start"]["turn_id"].as_str().unwrap();
        let turn_out = session
            .run_command(&["--json", "session", "turn", first_tid])
            .unwrap();
        evidence.record_command("查询首轮轮次详情（--json session turn）", &turn_out);
        assert!(turn_out.is_clean_exit(0));
        let first_detail: serde_json::Value = serde_json::from_slice(&turn_out.stdout).unwrap();
        assert_eq!(first_detail["summary"]["outcome"], "completed");
        assert_eq!(first_detail["steps"].as_array().unwrap().len(), 2);
        assert_eq!(first_detail["steps"][0]["slots"][0]["outcome"], "succeeded");
        assert_eq!(
            first_detail["steps"][0]["slots"][0]["binding"]["method"],
            "record_note"
        );
        assert_eq!(
            first_detail["steps"][0]["slots"][0]["binding"]["tool_identity"],
            "fake:record_note@1"
        );
        record_pty_session(
            &session,
            &mut evidence,
            sid,
            serde_json::json!({"prompt":"ping","script":"record_note fake tool step + three-line text step","exit":"/quit"}),
        );
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

mod support;

use support::record_session as record_pty_session;

#[cfg(feature = "fake-tools")]
#[test]
fn pty_statusline_usage_survives_reopening_and_follows_rewind() {
    use lattice_system_harness::{NativeUsage, SessionSendDriver, SseScript};
    use serde_json::{Value, json};

    fn query(
        session: &SessionSendDriver<'_>,
        evidence: &mut lattice_system_harness::ScenarioEvidence,
        args: &[&str],
    ) -> Value {
        let out = session.run_command(args).unwrap();
        evidence.record_command(format!("查询会话状态（{}）", args.join(" ")), &out);
        assert!(out.is_clean_exit(0), "{}", out.summary());
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn wait_for_usage(
        tui: &mut PtyTuiDriver,
        expected: Option<[f64; 5]>,
        timeout: Duration,
    ) -> Option<[f64; 5]> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let frame = tui
                .redraw_text(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            let number = |prefix| {
                frame
                    .split_once(prefix)?
                    .1
                    .split_whitespace()
                    .next()?
                    .trim_end_matches('%')
                    .parse::<f64>()
                    .ok()
            };
            let shown = (|| {
                Some([
                    number("↑")?,
                    number("↓")?,
                    number("缓存 ")?,
                    number("≈$")?,
                    number("上下文 ")?,
                ])
            })();
            let cleared = ["↑", "↓", "缓存", "$", "上下文"]
                .iter()
                .all(|label| !frame.contains(label));
            if shown == expected && (expected.is_some() || cleared) {
                return shown;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "计量未到达 {expected:?}，最近一帧：{frame}"
            );
        }
    }

    let driver = driver();
    let (world, mut spec) = support::direct_world(
        "pty-statusline-usage",
        "m",
        "[llm.retry]\nmax_retries = 1\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    world.set_env("LATTICE_TEST_TOOLS", "record_usage");
    spec.layer = ScenarioLayer::PtySmoke;
    spec.step_timeout_secs = 120;
    spec.total_timeout_secs = Some(300);
    let budget = world.budget(&spec);
    let step = || budget.cap_timeout(spec.step_timeout());
    world.start_daemon().expect("显式启动带测试工具的后台");
    let usage = |input, output, cached| json!({"prompt_tokens":input,"completion_tokens":output,"prompt_tokens_details":{"cached_tokens":cached}});
    let with_usage = |script, value| SseScript::WithUsage {
        script: Box::new(script),
        usage: NativeUsage::ChatCompletions(value),
    };
    let (server, gate) = support::wire_gated_fake_llm(
        &world,
        "m",
        support::sequence(vec![
            SseScript::Chunks {
                chunks: vec![
                    json!({"choices":[{"delta":{"content":"retry usage"}}],"usage":usage(100,10,100)}),
                    json!({"error":{"type":"server_error","message":"retry once"}}),
                ],
            },
            with_usage(
                support::tool_call(
                    "usage-tool",
                    "record_usage",
                    r#"{"action":"read","value":{"step":"usage"}}"#,
                ),
                usage(200, 20, 0),
            ),
            with_usage(SseScript::plain("saved usage answer"), usage(300, 30, 200)),
        ]),
    );
    let configuration = world.home().join(".lattice/providers.toml");
    let baseline = std::fs::read_to_string(&configuration).unwrap();
    std::fs::write(&configuration, format!("{baseline}\n[providers.local.models.m.cost]\ninput = 100\noutput = 1000\ncache_read = 0\n")).unwrap();
    let session = SessionSendDriver::new(&driver, &world, &spec, "m");
    let mut evidence = support::evidence(&world, &spec);
    let mut tui = PtyTuiDriver::spawn(&driver, &world, "m").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.wait_for_ready(step()).unwrap();
        tui.send_line("measure this turn").unwrap();
        for request in 1..=3 {
            gate.wait_until_stalled(request, step()).unwrap();
            if request == 1 {
                tui.wait_for(step(), |text| text.contains("$?")).unwrap();
            }
            gate.release_next(step()).unwrap();
        }
        tui.wait_for(step(), |text| {
            text.contains("saved usage answer") && text.contains("完成")
        })
        .unwrap();
        wait_pty_frame(&mut tui, step(), |frame| {
            frame.contains("▸ ✓ record_usage · 16 B") && frame.contains("saved usage answer")
        });
        let initial = wait_for_usage(&mut tui, Some([600.0, 60.0, 50.0, 0.09, 330.0]), step());
        let list = query(&session, &mut evidence, &["--json", "session", "list"]);
        assert_eq!(list.as_array().unwrap().len(), 1);
        let sid = list[0]["session_id"].as_str().unwrap();
        let persisted = query(
            &session,
            &mut evidence,
            &["--json", "session", "usage", sid],
        );
        assert_eq!(persisted["path_total"]["attempts"], 3);
        assert_eq!(persisted["path_total"]["input"]["known"], 600);
        assert_eq!(persisted["path_total"]["output"]["known"], 60);
        assert_eq!(persisted["path_total"]["cache_read"]["known"], 300);
        assert!(
            (persisted["path_total"]["cost"]["known"]["usd"]
                .as_f64()
                .unwrap()
                - 0.09)
                .abs()
                < 1e-12
        );
        assert_eq!(persisted["path_total"]["cost"]["known"]["exact"], false);
        assert_eq!(persisted["session_total"], persisted["path_total"]);
        let tid = persisted["turns"][0]["turn_id"].as_str().unwrap();
        let detail = query(&session, &mut evidence, &["--json", "session", "turn", tid]);
        assert_eq!(detail["steps"].as_array().unwrap().len(), 2);
        assert_eq!(detail["steps"][0]["slots"][0]["outcome"], "succeeded");
        assert_eq!(
            detail["steps"][0]["slots"][0]["binding"]["method"],
            "record_usage"
        );
        let middle = detail["steps"][0]["node"].as_u64().unwrap().to_string();
        // 释放重试的 `Warn`，再正常退出；后台也停止，重开只能读取已保存计量。
        tui.send_keys(b" ").unwrap();
        tui.send_line("/quit").unwrap();
        tui.wait_for_exit(step()).unwrap();
        evidence.record_pty(tui.output());
        world
            .restart_daemon()
            .expect("显式重启后台后再重连终端界面");

        let mut reopened = PtyTuiDriver::reconnect(&driver, &world, sid).unwrap();
        let reload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reopened.wait_for_ready(step()).unwrap();
            // 备用屏幕先于终端尺寸初始化；等初始内容绘出后再调整窗口大小，才能触发清屏重绘。
            reopened
                .wait_for(step(), |text| {
                    text.contains("saved usage answer") && text.contains("完成")
                })
                .unwrap();
            let reloaded = wait_for_usage(&mut reopened, initial, step());
            assert_eq!(
                query(
                    &session,
                    &mut evidence,
                    &["--json", "session", "usage", sid]
                ),
                persisted
            );
            query(
                &session,
                &mut evidence,
                &["--json", "session", "rewind", sid, &middle],
            );
            let rewound = wait_for_usage(
                &mut reopened,
                Some([300.0, 30.0, 33.0, 0.05, 220.0]),
                step(),
            );
            let path = query(
                &session,
                &mut evidence,
                &["--json", "session", "usage", sid],
            );
            assert_eq!(path["path_total"]["attempts"], 2);
            assert_eq!(path["path_total"]["input"]["known"], 300);
            assert_eq!(path["session_total"], persisted["session_total"]);
            assert_eq!(
                query(&session, &mut evidence, &["--json", "session", "turn", tid]),
                detail
            );
            query(
                &session,
                &mut evidence,
                &["--json", "session", "rewind", sid, "0"],
            );
            let root = wait_for_usage(&mut reopened, None, step());
            let empty = query(
                &session,
                &mut evidence,
                &["--json", "session", "usage", sid],
            );
            assert!(empty["turns"].as_array().unwrap().is_empty());
            evidence
                .record_json(
                    "statusline-values",
                    &json!({"initial":initial,"reopened":reloaded,"rewound":rewound,"root":root}),
                )
                .unwrap();
            reopened.send_line("/quit").unwrap();
            reopened.wait_for_exit(step()).unwrap();
        }));
        evidence.record_pty(reopened.output());
        if let Err(error) = reload {
            std::panic::resume_unwind(error);
        }
        assert_eq!(
            server.captured_requests().len(),
            3,
            "查询、重开与退回不产生模型请求"
        );
    }));
    evidence.record_pty(tui.output());
    evidence.record_captures(server.captured_requests());
    drop(tui);
    support::finish_recorded(evidence, result);
}

fn finish_pty(
    evidence: lattice_system_harness::ScenarioEvidence,
    result: std::thread::Result<()>,
) -> Result<(), lattice_system_harness::FailureReport> {
    let primary = result.err().map(|error| {
        error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_else(|| "PTY 场景以非文本载荷触发了 panic".into())
    });
    evidence.finish_report(primary)
}

#[test]
fn pty_failure_report_preserves_redacted_panic_reason() {
    for (index, payload) in [
        Box::new("特定 PTY 等待失败 FAKE_SECRET") as Box<dyn std::any::Any + Send>,
        Box::new(String::from("特定 PTY 结果失败 FAKE_SECRET")),
    ]
    .into_iter()
    .enumerate()
    {
        let world = ScenarioWorld::new("pty-panic-report").unwrap();
        let evidence = lattice_system_harness::ScenarioEvidence::new(
            &world,
            &format!("pty-panic-{index}"),
            ["FAKE_SECRET".into()],
        )
        .unwrap();
        let report = finish_pty(evidence, Err(payload)).unwrap_err();
        let rendered = report.to_string();
        assert!(rendered.contains("特定 PTY"), "{rendered}");
        assert!(!rendered.contains("FAKE_SECRET"), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
        println!("{rendered}");
    }
}
#[test]
fn pty_detach_reloads_pending_and_result_without_cancelling() {
    use lattice_system_harness::{SessionSendDriver, SseScript};
    let driver = driver();
    let (world, mut spec) = support::direct_world("pty-background-reconnect", "tui-smoke", "");
    spec.step_timeout_secs = 120;
    spec.total_timeout_secs = Some(300);
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    world.start_daemon().expect("显式启动后台");
    let (server, gate) = support::wire_gated_fake_llm(
        &world,
        "tui-smoke",
        SseScript::plain("background reconnect answer"),
    );
    let session = SessionSendDriver::new(&driver, &world, &spec, "tui-smoke");
    let mut tui = PtyTuiDriver::spawn(&driver, &world, "tui-smoke").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.wait_for_ready(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        tui.send_line("persistent pending input").unwrap();
        gate.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let listed = session.run_command(&["--json", "session", "list"]).unwrap();
        evidence.record_command("查询已接受轮次的 TUI 会话列表", &listed);
        assert!(listed.is_clean_exit(0));
        let list: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        let sid = list[0]["session_id"].as_str().unwrap();
        let owners = world.daemon_pids().unwrap();
        assert_eq!(owners.len(), 1);
        assert_ne!(tui.pgid(), Some(owners[0]));
        tui.send_keys(b"\x04").unwrap();
        tui.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
            .unwrap();
        assert!(processes_in_pgid(tui.pgid().unwrap()).unwrap().is_empty());
        assert_eq!(world.daemon_pids().unwrap(), owners);
        evidence.record_pty(tui.output());
        let mut reconnected = PtyTuiDriver::reconnect(&driver, &world, sid).unwrap();
        let result = (|| {
            reconnected.wait_for_ready(budget.cap_timeout(spec.step_timeout()))?;
            reconnected.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("persistent pending input")
            })?;
            gate.release(budget.cap_timeout(spec.step_timeout()))
                .unwrap();
            reconnected.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("background reconnect answer")
            })?;
            reconnected.send_line("/quit")?;
            reconnected.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
        })();
        evidence.record_pty(reconnected.output());
        result.unwrap();
        world
            .restart_daemon()
            .expect("显式重启后台后再重连终端界面");
        let mut reopened = PtyTuiDriver::reconnect(&driver, &world, sid).unwrap();
        let reopened_result = (|| {
            reopened.wait_for_ready(budget.cap_timeout(spec.step_timeout()))?;
            reopened.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                text.contains("background reconnect answer") && text.contains("完成")
            })?;
            reopened.send_line("/quit")?;
            reopened.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
        })();
        evidence.record_pty(reopened.output());
        reopened_result.unwrap();
        assert_eq!(server.captured_requests().len(), 1);
        record_pty_session(
            &session,
            &mut evidence,
            sid,
            serde_json::json!({"script":"gated background reconnect answer","frontend":"Ctrl+D while provider held","daemon_pids":owners,"provider_requests":1}),
        );
    }));
    evidence.record_pty(tui.output());
    evidence.record_captures(server.captured_requests());
    drop(tui);
    support::finish_recorded(evidence, result);
}

/// 真实模型段已开始后切换前端：新后缀必须在终态门闩放行前可见。
#[test]
fn pty_mid_model_reconnect_shows_new_suffix_before_completion() {
    use lattice_system_harness::{FakeLlmServer, SessionSendDriver, SseScript};
    const PREFIX: &str = "SAVED_PREFIX_R1";
    const OLD: &str = "OLD_DELTA_R1";
    const NEW: &str = "NEW_SUFFIX_R1";
    const INPUT: &str = "continue mid-model";
    let driver = driver();
    let (world, mut spec) = support::direct_world("pty-mid-model-reconnect", "tui-smoke", "");
    spec.layer = ScenarioLayer::PtySmoke;
    spec.step_timeout_secs = 120;
    spec.total_timeout_secs = Some(300);
    let budget = world.budget(&spec);
    let step = || budget.cap_timeout(spec.step_timeout());
    let mut evidence = support::evidence(&world, &spec);
    world.start_daemon().expect("显式启动后台");
    let (mut server, gate) = FakeLlmServer::start_segmented(
        SseScript::Sequence {
            scripts: vec![
                SseScript::plain(PREFIX),
                SseScript::Chunks {
                    chunks: vec![
                        serde_json::json!({"choices":[{"index":0,"delta":{"content":OLD}}]}),
                        serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":"HIDDEN_REASONING_S4"}}]}),
                        serde_json::json!({"choices":[{"index":0,"delta":{"content":NEW}}]}),
                        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                    ],
                },
            ],
        },
        step(),
    )
    .unwrap();
    world
        .wire_provider(
            ProviderFixture::chat_completions("tui-smoke"),
            &server,
            lattice_system_harness::FAKE_API_KEY,
        )
        .unwrap();
    let mut original = None;
    let mut replacement = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let session = SessionSendDriver::new(&driver, &world, &spec, "tui-smoke");
        let seed = session.spawn("seed history").unwrap();
        // 第一请求的正文、`finish`、`[DONE]`；第二请求仍独立停在下一帧门闩。
        for frame in 1..=3 {
            gate.wait_until_stalled(frame, step()).unwrap();
            gate.release_next(step()).unwrap();
        }
        let seed = session.wait_spawned(seed).unwrap();
        evidence.record_command("建立持久历史前缀", &seed.command);
        support::assert_clean_send(&seed).unwrap();
        assert!(seed.command.is_clean_exit(0));
        support::assert_accepted_and_closed(&seed.parsed).unwrap();

        // 提交端的接受回执会触发重固定；不要把它误当成 TUI 已完成订阅。
        // 先通过公开 CLI 建立在途 Turn，两次 TUI 都以只读前端进入同一模型段。
        let in_flight = session.spawn(INPUT).unwrap();
        gate.wait_until_stalled(4, step()).unwrap();
        original =
            Some(PtyTuiDriver::reconnect(&driver, &world, session.session_id().unwrap()).unwrap());
        let tui = original.as_mut().unwrap();
        tui.wait_for_ready(step()).unwrap();
        tui.wait_for(step(), |text| {
            text.contains(INPUT) && text.contains(PREFIX) && text.contains("stream")
        })
        .unwrap();
        gate.release_next(step()).unwrap();
        gate.wait_until_stalled(5, step()).unwrap();
        tui.wait_for(step(), |text| text.contains(OLD)).unwrap();
        let old_frame = tui.redraw_text(step()).unwrap();
        assert!(
            old_frame.contains(OLD) && !old_frame.contains(NEW),
            "{old_frame}"
        );
        println!("PTY 观察窗口：旧片段已交付并可见，新片段仍暂扣在门闩后");

        let owners = world.daemon_pids().unwrap();
        assert_eq!(owners.len(), 1);
        assert_ne!(tui.pgid(), Some(owners[0]));
        tui.send_keys(b"\x04").unwrap();
        tui.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
            .unwrap();
        assert!(processes_in_pgid(tui.pgid().unwrap()).unwrap().is_empty());
        assert_eq!(world.daemon_pids().unwrap(), owners);
        println!("PTY 观察窗口：原前端已退出，同一后台保持运行");

        // 重连读取不依赖当前凭据，且不得触发第二次模型执行。
        world
            .register_key(
                lattice_system_harness::ProviderFixture::chat_completions("reconnect"),
                None,
            )
            .unwrap();
        replacement =
            Some(PtyTuiDriver::reconnect(&driver, &world, session.session_id().unwrap()).unwrap());
        let tui = replacement.as_mut().unwrap();
        tui.wait_for_ready(step()).unwrap();
        tui.wait_for(step(), |text| {
            text.contains(INPUT) && text.contains(PREFIX) && text.contains("stream")
        })
        .unwrap();
        let resumed = tui.redraw_text(step()).unwrap();
        assert!(
            resumed.contains(INPUT) && resumed.contains("stream") && !resumed.contains("offline"),
            "{resumed}"
        );
        assert_eq!(resumed.matches(PREFIX).count(), 1, "{resumed}");
        assert!(resumed.contains(OLD) && !resumed.contains(NEW), "{resumed}");
        println!("PTY 观察窗口：接替前端已订阅且仅加载一次持久前缀");

        gate.release_next(step()).unwrap();
        gate.wait_until_stalled(6, step()).unwrap();
        gate.release_next(step()).unwrap();
        gate.wait_until_stalled(7, step()).unwrap();
        tui.wait_for(step(), |text| text.contains(NEW) && text.contains(OLD))
            .unwrap();
        let live = tui.redraw_text(step()).unwrap();
        assert!(live.contains(NEW) && live.contains(OLD), "{live}");
        assert!(live.contains(&format!("{OLD}{NEW}")), "{live}");
        assert!(!live.contains("HIDDEN_REASONING_S4"), "{live}");
        assert_eq!(live.matches(PREFIX).count(), 1, "{live}");
        assert_eq!(live.matches(NEW).count(), 1, "{live}");
        let pending = session
            .run_command(&["--json", "session", "log", session.session_id().unwrap()])
            .unwrap();
        evidence.record_command("持久完成前已可见新后缀时的会话日志", &pending);
        assert!(pending.is_clean_exit(0));
        let log: serde_json::Value = serde_json::from_slice(&pending.stdout).unwrap();
        assert!(!log["unclosed_turn"].is_null(), "当前轮次仍应处于运行中");
        assert_eq!(log["nodes"].as_array().unwrap().len(), 5);
        assert_eq!(server.captured_requests().len(), 2);
        println!("PTY 观察窗口：完整前缀与新后缀均已可见，终态帧仍暂扣");

        gate.release(step()).unwrap();
        tui.wait_for(step(), |text| text.contains(OLD)).unwrap();
        let sent = session.wait_spawned(in_flight).unwrap();
        evidence.record_command("原 CLI 提交已完成", &sent.command);
        support::assert_clean_send(&sent).unwrap();
        assert!(sent.command.is_clean_exit(0));
        support::assert_accepted_and_closed(&sent.parsed).unwrap();
        let closed = session
            .run_command(&["--json", "session", "log", session.session_id().unwrap()])
            .unwrap();
        evidence.record_command("终态帧放行后原轮次完成", &closed);
        assert!(closed.is_clean_exit(0));
        let log: serde_json::Value = serde_json::from_slice(&closed.stdout).unwrap();
        let nodes = log["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 5);
        let completed: Vec<_> = nodes
            .iter()
            .filter(|row| row["turn_start"]["turn_id"].as_str() == sent.parsed.accepted_turn_id())
            .collect();
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0]["turn_start"]["summary"]["outcome"],
            "completed"
        );
        assert_eq!(server.captured_requests().len(), 2);
        let log_text = session
            .run_command(&["session", "log", session.session_id().unwrap()])
            .unwrap();
        evidence.record_command("查询会话日志（session log）", &log_text);
        assert!(log_text.is_clean_exit(0), "{}", log_text.summary());
        let turn_out = session
            .run_command(&[
                "--json",
                "session",
                "turn",
                sent.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("查询轮次详情（--json session turn）", &turn_out);
        assert!(turn_out.is_clean_exit(0), "{}", turn_out.summary());
        let detail: serde_json::Value = serde_json::from_slice(&turn_out.stdout).unwrap();
        assert_eq!(detail["session_id"], session.session_id().unwrap());
        assert_eq!(detail["turn_id"], sent.accepted_turn_id().unwrap());
        assert_eq!(detail["summary"]["outcome"], "completed");
        assert_eq!(detail["steps"].as_array().unwrap().len(), 1);
        assert_eq!(
            detail["steps"][0]["end"],
            serde_json::json!({"end": "finish"})
        );
        assert_eq!(detail["steps"][0]["attempts"].as_array().unwrap().len(), 1);
        assert!(detail["steps"][0]["slots"].as_array().unwrap().is_empty());
        assert!(
            support::same_turn(&support::live_turn(&sent.parsed), &detail),
            "实时条目与 session turn --json 持久详情一致：{detail}"
        );
        evidence
            .record_json(
                "observation-windows",
                &serde_json::json!({
                    "root":world.root(),"workspace":world.project_dir(),
                    "state_root":world.session_store_dir().parent(),
                    "daemon_pids":owners,"frontend_pgid":tui.pgid(),
                    "original":old_frame,"resumed":resumed,"live":live,
                    "gates_hit":[4,5,6,7],"provider_requests":2
                }),
            )
            .unwrap();
        tui.send_line("/quit").unwrap();
        tui.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
            .unwrap();
        assert!(processes_in_pgid(tui.pgid().unwrap()).unwrap().is_empty());
        let mut reopened =
            PtyTuiDriver::reconnect(&driver, &world, session.session_id().unwrap()).unwrap();
        let reload_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reopened.wait_for_ready(step()).unwrap();
            reopened
                .wait_for(step(), |text| text.contains(OLD) && text.contains("完成"))
                .unwrap();
            let reloaded = reopened.redraw_text(step()).unwrap();
            evidence
                .record_json("reloaded-frame", &serde_json::json!({"text":reloaded}))
                .unwrap();
            assert!(reloaded.contains(&format!("{OLD}{NEW}")), "{reloaded}");
            assert!(!reloaded.contains("HIDDEN_REASONING_S4"), "{reloaded}");
            assert_eq!(reloaded.matches(OLD).count(), 1, "{reloaded}");
            assert_eq!(reloaded.matches(NEW).count(), 1, "{reloaded}");
            reopened.send_line("/quit").unwrap();
            reopened
                .wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
                .unwrap();
        }));
        evidence.record_pty(reopened.output());
        drop(reopened);
        if let Err(error) = reload_result {
            std::panic::resume_unwind(error);
        }
        server
            .shutdown(budget.cap_timeout(Duration::from_secs(3)))
            .unwrap();
        println!("PTY 观察窗口：原轮次已完成，接替前端干净退出，provider 请求数保持为两次");
    }));
    for tui in [original.as_ref(), replacement.as_ref()]
        .into_iter()
        .flatten()
    {
        evidence.record_pty(tui.output());
    }
    evidence.record_captures(server.captured_requests());
    // 失败时先解除假 provider 与 PTY，再让场景证据按既有预算停止自有后台。
    drop(server);
    drop(replacement);
    drop(original);
    support::finish_recorded(evidence, result);
}

#[test]
fn pty_second_ctrl_c_preserves_cancel_acknowledgement() {
    use lattice_system_harness::{SessionSendDriver, SseScript};
    let driver = driver();
    let (world, mut spec) = support::direct_world("pty-background-cancel", "tui-smoke", "");
    spec.step_timeout_secs = 120;
    spec.total_timeout_secs = Some(300);
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    world.start_daemon().expect("显式启动后台");
    let (server, gate) = support::wire_gated_fake_llm(
        &world,
        "tui-smoke",
        SseScript::plain("must not be delivered"),
    );
    let session = SessionSendDriver::new(&driver, &world, &spec, "tui-smoke");
    let mut tui = PtyTuiDriver::spawn(&driver, &world, "tui-smoke").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.wait_for_ready(budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        tui.send_line("cancel before exit").unwrap();
        gate.wait_until_stalled(1, budget.cap_timeout(spec.step_timeout()))
            .unwrap();
        let listed = session.run_command(&["--json", "session", "list"]).unwrap();
        evidence.record_command("查询待取消轮次所在的会话列表", &listed);
        assert!(listed.is_clean_exit(0));
        let list: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        let sid = list[0]["session_id"].as_str().unwrap();
        tui.send_keys(b"\x03\x03").unwrap();
        tui.wait_for_exit(budget.cap_timeout(Duration::from_secs(15)))
            .unwrap();
        assert!(processes_in_pgid(tui.pgid().unwrap()).unwrap().is_empty());
        let deadline = std::time::Instant::now() + budget.cap_timeout(Duration::from_secs(30));
        loop {
            let log = session
                .run_command(&["--json", "session", "log", sid])
                .unwrap();
            evidence.record_command("TUI 退出后轮询取消收尾日志", &log);
            assert!(log.is_clean_exit(0));
            let value: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
            if value["unclosed_turn"].is_null() {
                let turn = &value["nodes"][1]["turn_start"];
                assert_eq!(turn["summary"]["outcome"], "cancelled");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "已取消的轮次未在截止时间前关闭"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(server.captured_requests().len(), 1);
        record_pty_session(
            &session,
            &mut evidence,
            sid,
            serde_json::json!({"script":"gated must not be delivered","frontend":"two Ctrl+C after accepted","provider_requests":1}),
        );
    }));
    evidence.record_pty(tui.output());
    evidence.record_captures(server.captured_requests());
    drop(tui);
    support::finish_recorded(evidence, result);
}

/// 每次读取本次窗口尺寸调整产生的完整帧，不从累计输出推断当前面板。
#[cfg(feature = "fake-tools")]
#[track_caller]
fn wait_pty_frame(
    tui: &mut PtyTuiDriver,
    timeout: Duration,
    predicate: impl Fn(&str) -> bool,
) -> String {
    let deadline = std::time::Instant::now() + timeout;
    let mut previous = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!remaining.is_zero(), "等待当前帧超时：{previous}");
        let frame = match tui.redraw_text(remaining) {
            Ok(frame) => frame,
            Err(error) => panic!("完整帧读取失败：{error}；最近一帧：{previous}"),
        };
        if predicate(&frame) {
            return frame;
        }
        previous = frame;
        std::thread::sleep(
            Duration::from_millis(75)
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
}

#[cfg(feature = "fake-tools")]
#[test]
fn pty_model_effort_selection_survives_retry_tools_and_reopening() {
    use lattice_system_harness::{FakeLlmServer, ProviderTarget, SessionSendDriver, SseScript};
    use serde_json::{Value, json};
    fn query(
        session: &SessionSendDriver<'_>,
        evidence: &mut lattice_system_harness::ScenarioEvidence,
        args: &[&str],
    ) -> Value {
        let result = session.run_command(args).unwrap();
        evidence.record_command(format!("查询会话状态（{}）", args.join(" ")), &result);
        assert!(result.is_clean_exit(0), "{}", result.summary());
        serde_json::from_slice(&result.stdout).unwrap()
    }
    let driver = driver();
    let (world, mut spec) = support::direct_world("pty-model-effort", "model-panel", "");
    world.set_env("LATTICE_TEST_TOOLS", "read_panel_note");
    spec.layer = ScenarioLayer::PtySmoke;
    spec.step_timeout_secs = 120;
    spec.total_timeout_secs = Some(300);
    let budget = world.budget(&spec);
    let step = || budget.cap_timeout(spec.step_timeout());
    world.start_daemon().expect("显式启动带测试工具的后台");
    let profile = "schema_version=1\n[agent]\nname='model-panel'\neffort='low'\n[prompt]\nsystem='Read panel note using read_panel_note.'\n[llm.retry]\nmax_retries=1\ninitial_delay_ms=1\nmax_delay_ms=1\n";
    world
        .write_project_file(".lattice/agents/model-panel/agent.toml", profile)
        .unwrap();
    let (server, gate) = FakeLlmServer::start_gated(support::sequence(vec![
        SseScript::HttpError {
            status: 503,
            body: "panel retry".into(),
        },
        support::tool_call(
            "panel-read",
            "read_panel_note",
            r#"{"action":"read","value":{"marker":"panel-file-marker"}}"#,
        ),
        SseScript::plain("panel tool complete"),
        SseScript::plain("panel inherited complete"),
        SseScript::plain("panel reopened complete"),
    ]))
    .unwrap();
    world
        .wire_provider(
            ProviderFixture::chat_completions("base"),
            &server,
            lattice_system_harness::FAKE_API_KEY,
        )
        .unwrap();
    world
        .wire_provider(
            ProviderFixture {
                target: ProviderTarget::Custom("openai-chat"),
                ..ProviderFixture::chat_completions("gpt-6-luna")
            },
            &server,
            lattice_system_harness::FAKE_API_KEY,
        )
        .unwrap();
    let configuration = world.home().join(".lattice/providers.toml");
    let mut providers: toml::Table =
        toml::from_str(&std::fs::read_to_string(&configuration).unwrap()).unwrap();
    providers["providers"]["openai-chat"]
        .as_table_mut()
        .unwrap()
        .insert("catalog".into(), "openai".into());
    let base = toml::to_string(&providers).unwrap();
    std::fs::write(&configuration, format!("{base}\n[providers.local.models.base]\neffort=['low']\n[providers.local.models.preview]\neffort=['low']\n")).unwrap();
    let cache = world.home().join(".lattice/cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("models.json"), serde_json::to_vec(&json!({"openai":{"models":{
        "gpt-6-luna":{"id":"gpt-6-luna","reasoning_options":[{"type":"effort","values":["none","low"]}]}
    }}})).unwrap()).unwrap();
    let session = SessionSendDriver::new(&driver, &world, &spec, "model-panel");
    let mut evidence = support::evidence(&world, &spec);
    let mut tui = PtyTuiDriver::spawn(&driver, &world, "model-panel").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.wait_for_ready(step()).unwrap();
        tui.wait_for(step(), |text| text.contains("显示命令"))
            .unwrap();
        tui.send_line("/effort").unwrap();
        wait_pty_frame(&mut tui, step(), |text| text.contains("先用 /model"));
        tui.send_keys(b"\x1b").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            !text.contains("EFFORT") && text.contains("USER")
        });
        tui.send_line("/model").unwrap();
        wait_pty_frame(&mut tui, step(), |text| text.contains("MODEL"));
        tui.send_keys(b"openai-chat/gpt-6-luna").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.matches("openai-chat/gpt-6-luna").count() == 2
        });
        tui.send_keys(b"\r").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            !text.contains("MODEL") && text.contains("gpt-6-luna")
        });
        tui.send_line("/effort").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("EFFORT") && text.contains("off") && text.contains("low")
        });
        tui.send_keys(b"off").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("EFFORT") && !text.contains("low · 当前")
        });
        tui.send_keys(b"\r").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            !text.contains("EFFORT") && text.contains("off")
        });
        assert!(server.captured_requests().is_empty());
        tui.send_line("read the panel file").unwrap();
        gate.wait_until_stalled(1, step()).unwrap();
        let listed = query(&session, &mut evidence, &["--json", "session", "list"]);
        assert_eq!(listed.as_array().unwrap().len(), 1);
        let sid = listed[0]["session_id"].as_str().unwrap();
        let head = support::ipc(&world, step(), json!({"operation":"head","session_id":sid}));
        assert_eq!(head["response"], "head");
        assert_eq!(head["value"]["model"], "gpt-6-luna");
        assert_eq!(head["value"]["provider"], "openai-chat");
        assert_eq!(head["value"]["effort"], "off");
        assert!(head["value"]["settings_version"].as_str().is_some());
        let first = head["value"]["turn_id"].as_str().unwrap();
        evidence.record_json("selected-head", &head).unwrap();
        tui.send_line("/model").unwrap();
        wait_pty_frame(&mut tui, step(), |text| text.contains("MODEL"));
        tui.send_keys(b"local/preview").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.matches("local/preview").count() == 2
        });
        gate.release_next(step()).unwrap();
        gate.wait_until_stalled(2, step()).unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("WARN") && !text.contains("MODEL")
        });
        tui.send_keys(b"ignored\r").unwrap();
        tui.send_keys(b" ").unwrap();
        let restored = wait_pty_frame(&mut tui, step(), |text| {
            text.contains("MODEL") && text.matches("local/preview").count() == 2
        });
        assert!(!restored.contains("ignored"));
        // 一轮执行中用 `/model` 选定新模型：屏幕上出现 `Warn` 说明执行中不能修改设置，会话头不变。
        tui.send_keys(b"\r").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            !text.contains("MODEL")
                && text.contains("WARN")
                && text.contains("这一轮执行中不能修改设置")
        });
        let busy_head = support::ipc(&world, step(), json!({"operation":"head","session_id":sid}));
        assert_eq!(busy_head, head);
        tui.send_keys(b" ").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            !text.contains("WARN") && text.contains("gpt-6-luna") && text.contains("off")
        });
        gate.release_next(step()).unwrap();
        gate.wait_until_stalled(3, step()).unwrap();
        assert_eq!(
            support::tool_result_value(&server.captured_requests()[2]),
            Some(json!({"marker": "panel-file-marker"}))
        );
        gate.release_next(step()).unwrap();
        let panel_done = wait_pty_frame(&mut tui, step(), |text| {
            text.contains("panel tool complete") && text.contains("idle")
        });
        assert!(
            panel_done.contains("▸ ✓ read_panel_note · 30 B"),
            "假工具结果应在 TUI 中按实际字节长度显示：{panel_done}"
        );
        let detail = query(
            &session,
            &mut evidence,
            &["--json", "session", "turn", first],
        );
        assert_eq!(detail["summary"]["outcome"], "completed");
        assert_eq!(detail["steps"][0]["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(detail["steps"][0]["slots"][0]["outcome"], "succeeded");
        assert_eq!(
            detail["steps"][0]["slots"][0]["binding"]["method"],
            "read_panel_note"
        );
        assert_eq!(
            detail["steps"][0]["slots"][0]["binding"]["tool_identity"],
            "fake:read_panel_note@1"
        );
        tui.send_line("inherit the accepted selection").unwrap();
        gate.wait_until_stalled(4, step()).unwrap();
        gate.release_next(step()).unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("panel inherited complete") && text.contains("idle")
        });
        let before = support::ipc(&world, step(), json!({"operation":"head","session_id":sid}));
        let before_log = query(&session, &mut evidence, &["--json", "session", "log", sid]);
        tui.send_line("/quit").unwrap();
        tui.wait_for_exit(step()).unwrap();
        assert!(processes_in_pgid(tui.pgid().unwrap()).unwrap().is_empty());
        evidence.record_pty(tui.output());
        world
            .restart_daemon()
            .expect("显式重启后台后再重连终端界面");
        tui = PtyTuiDriver::reconnect(&driver, &world, sid).unwrap();
        tui.wait_for_ready(step()).unwrap();
        tui.wait_for(step(), |text| text.contains("panel inherited complete"))
            .unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("gpt-6-luna") && text.contains("off") && text.contains("idle")
        });
        assert_eq!(server.captured_requests().len(), 4);
        // 轮次结束后切换到不支持当前档位 `off` 的模型：出现档位调整 `Info`，状态行显示新模型与档位。
        tui.send_line("/model").unwrap();
        wait_pty_frame(&mut tui, step(), |text| text.contains("MODEL"));
        tui.send_keys(b"local/preview").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.matches("local/preview").count() == 2
        });
        tui.send_keys(b"\r").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            !text.contains("MODEL")
                && text.contains("换模型后推理档位由 off 改为 low")
                && text.contains("preview · local · low")
        });
        let preview_head =
            support::ipc(&world, step(), json!({"operation":"head","session_id":sid}));
        assert_eq!(preview_head["value"]["model"], "preview");
        assert_eq!(preview_head["value"]["provider"], "local");
        assert_eq!(preview_head["value"]["effort"], "low");
        assert_ne!(
            preview_head["value"]["settings_version"],
            before["value"]["settings_version"]
        );
        let after_configure_log =
            query(&session, &mut evidence, &["--json", "session", "log", sid]);
        let mut expected_log = before_log.clone();
        expected_log["session"]["updated_at"] =
            after_configure_log["session"]["updated_at"].clone();
        assert_eq!(after_configure_log, expected_log);
        tui.send_line("/effort").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("EFFORT") && text.contains("low · 当前")
        });
        tui.send_keys(b"\x1b").unwrap();
        wait_pty_frame(&mut tui, step(), |text| !text.contains("EFFORT"));
        tui.send_line("/quit").unwrap();
        tui.wait_for_exit(step()).unwrap();
        evidence.record_pty(tui.output());
        assert_eq!(server.captured_requests().len(), 4);
        assert_eq!(
            support::ipc(&world, step(), json!({"operation":"head","session_id":sid})),
            preview_head
        );
        assert_eq!(
            query(&session, &mut evidence, &["--json", "session", "log", sid]),
            after_configure_log
        );
        tui = PtyTuiDriver::reconnect(&driver, &world, sid).unwrap();
        tui.wait_for_ready(step()).unwrap();
        tui.wait_for(step(), |text| text.contains("panel inherited complete"))
            .unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("preview") && text.contains("low")
        });
        // 切回 `openai-chat/gpt-6-luna` 与 `off` 后再发第三轮。
        tui.send_line("/model").unwrap();
        wait_pty_frame(&mut tui, step(), |text| text.contains("MODEL"));
        tui.send_keys(b"openai-chat/gpt-6-luna").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.matches("openai-chat/gpt-6-luna").count() == 2
        });
        tui.send_keys(b"\r").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            !text.contains("MODEL") && text.contains("gpt-6-luna") && text.contains("low")
        });
        tui.send_line("/effort").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("EFFORT") && text.contains("off")
        });
        tui.send_keys(b"off").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("EFFORT") && !text.contains("low · 当前")
        });
        tui.send_keys(b"\r").unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            !text.contains("EFFORT") && text.contains("gpt-6-luna") && text.contains("off")
        });
        tui.send_line("after reconfiguring back").unwrap();
        gate.wait_until_stalled(5, step()).unwrap();
        gate.release_next(step()).unwrap();
        wait_pty_frame(&mut tui, step(), |text| {
            text.contains("panel reopened complete") && text.contains("idle")
        });
        let mut watcher = support::Watcher::open(&world, step(), sid);
        let view = watcher.initial();
        let users = view
            .entries
            .values()
            .filter(|entry| entry.raw["kind"] == "user_message")
            .collect::<Vec<_>>();
        assert_eq!(users.len(), 3);
        let final_head = view.head.as_ref().unwrap();
        let mut observed_turns = users
            .iter()
            .map(|user| user.raw["content"]["turn_id"].as_str().unwrap())
            .collect::<Vec<_>>();
        let mut accepted_turns = vec![
            first,
            before["value"]["turn_id"].as_str().unwrap(),
            final_head.turn_id.as_deref().unwrap(),
        ];
        observed_turns.sort_unstable();
        accepted_turns.sort_unstable();
        assert_eq!(
            observed_turns, accepted_turns,
            "三次提交与公开 UserMessage 的身份一致"
        );
        for user in users {
            assert_eq!(user.raw["content"]["provider"], "openai-chat");
            assert_eq!(user.raw["content"]["model"], "gpt-6-luna");
            assert_eq!(user.raw["content"]["effort"], "off");
        }
        assert_eq!(final_head.effort.as_deref(), Some("off"));
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 5);
        for capture in &captures {
            assert_eq!(capture.raw["model"], "gpt-6-luna");
            assert_eq!(capture.raw["reasoning_effort"], "none");
            assert!(
                capture
                    .tool_names
                    .iter()
                    .any(|tool| tool == "read_panel_note")
            );
            assert!(capture.raw.get("enable_thinking").is_none());
        }
        assert_eq!(
            std::fs::read_to_string(
                world
                    .project_dir()
                    .join(".lattice/agents/model-panel/agent.toml")
            )
            .unwrap(),
            profile
        );
        tui.send_line("/quit").unwrap();
        tui.wait_for_exit(step()).unwrap();
        assert!(processes_in_pgid(tui.pgid().unwrap()).unwrap().is_empty());
        drop(watcher);
        record_pty_session(
            &session,
            &mut evidence,
            sid,
            json!({"script":"503, read_panel_note fake tool, final, inherited, reopened", "provider_requests":5}),
        );
    }));
    evidence.record_pty(tui.output());
    evidence.record_captures(server.captured_requests());
    drop(tui);
    support::finish_recorded(evidence, result);
}
