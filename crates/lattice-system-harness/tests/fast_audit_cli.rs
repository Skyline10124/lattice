// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 无数据库时 operator 查询必须类型化失败且不创建库、不回显私有路径。
//! 有库查询、全局 ChangeSeq 与重放由 direct_operator 保护。

mod support;
use lattice_system_harness::{
    AssertError, CommandOutcome, LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld,
    assert_exit_code, assert_stderr_contains,
};
use serde_json::Value;

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

fn run(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> CommandOutcome {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
}

fn ok_or_artifacts(
    world: &ScenarioWorld,
    name: &str,
    summary: &str,
    out: &CommandOutcome,
    result: Result<(), AssertError>,
) {
    let mut evidence = lattice_system_harness::ScenarioEvidence::new(world, name, []).unwrap();
    evidence.record_command(summary, out);
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            result.unwrap();
            let exists = world.session_store_dir().join("lattice.redb").exists();
            assert!(!exists, "read must not initialize the store");
            evidence.record_json("state-summary", &serde_json::json!({
            "database_exists":exists,"log_turn":"不适用：未初始化库，没有 Session 或 Turn"
        })).unwrap();
            evidence
                .record_json(
                    "parameters",
                    &serde_json::json!({"command":summary,"profile":null,"provider":null}),
                )
                .unwrap();
        }));
    support::finish_recorded(evidence, result, "fast_audit_cli", name);
}

/// 缺少调用账本的公开诊断；文本模式与 JSON 模式共用同一原因。
const MISSING_LEDGER: &str = "session store unavailable";

fn require(condition: bool, expected: String) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing { expected })
    }
}

/// S2 的类型化缺库错误显式指出目标库；不回显无关 workspace。
fn no_private_path(world: &ScenarioWorld, out: &CommandOutcome) -> Result<(), AssertError> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let private = world.project_dir().display().to_string();
    require(
        !stdout.contains(&private) && !stderr.contains(&private),
        format!("diagnostics must not echo {private}: {}", out.summary()),
    )
}

/// 缺库文本错误写标准错误，返回退出码 1。
fn assert_missing_store(world: &ScenarioWorld, name: &str, summary: &str, out: &CommandOutcome) {
    let result = assert_exit_code(out, 1)
        .and_then(|_| assert_stderr_contains(out, MISSING_LEDGER))
        .and_then(|_| {
            require(
                out.stdout.is_empty(),
                format!("text errors stay off stdout: {}", out.summary()),
            )
        })
        .and_then(|_| no_private_path(world, out));
    ok_or_artifacts(world, name, summary, out, result);
}

/// JSON 类型化错误写标准输出，返回退出码 1，标准错误为空。
fn assert_missing_store_json(
    world: &ScenarioWorld,
    name: &str,
    summary: &str,
    out: &CommandOutcome,
) {
    let result = assert_exit_code(out, 1)
        .and_then(|_| {
            require(
                out.stderr.is_empty(),
                format!("JSON errors stay off stderr: {}", out.summary()),
            )
        })
        .and_then(|_| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let record: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
                AssertError::StdoutMissing {
                    expected: format!("one typed error record ({error}): {stdout}"),
                }
            })?;
            require(
                record.get("type").and_then(Value::as_str) == Some("error")
                    && record.get("kind").and_then(Value::as_str) == Some("store_unavailable")
                    && record["store"]["type"] == "not_initialized"
                    && record["store"]["path"]
                        == world
                            .session_store_dir()
                            .join("lattice.redb")
                            .to_str()
                            .unwrap()
                    && record
                        .get("message")
                        .and_then(Value::as_str)
                        .is_some_and(|message| message.contains(MISSING_LEDGER)),
                format!("unexpected audit JSON diagnostic: {stdout}"),
            )
        })
        .and_then(|_| no_private_path(world, out));
    ok_or_artifacts(world, name, summary, out, result);
}

#[test]
fn fast_audit_query_missing_store_errors_cleanly_json() {
    let spec = ScenarioSpec::fast("fast-audit-query-missing-json");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["audit", "query", "--json"]);
    assert_missing_store_json(
        &world,
        "fast_audit_query_missing_store_errors_cleanly_json",
        "lattice audit query --json",
        &out,
    );
}

#[test]
fn fast_audit_query_missing_store_errors_cleanly_text() {
    let spec = ScenarioSpec::fast("fast-audit-query-missing-text");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["audit", "query"]);
    assert_missing_store(
        &world,
        "fast_audit_query_missing_store_errors_cleanly_text",
        "lattice audit query",
        &out,
    );
}

#[test]
fn fast_audit_tail_missing_store_errors_cleanly() {
    let spec = ScenarioSpec::fast("fast-audit-tail-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["audit", "query", "--tail", "--json"]);
    assert_missing_store_json(
        &world,
        "fast_audit_tail_missing_store_errors_cleanly",
        "lattice audit query --tail --json",
        &out,
    );
}

#[test]
fn fast_audit_replay_missing_store_errors_cleanly() {
    let spec = ScenarioSpec::fast("fast-audit-replay-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(
        &world,
        &spec,
        &[
            "audit",
            "replay",
            "--session",
            "00000000000000000000000000000001",
            "--turn",
            "00000000-0000-4000-8000-000000000001",
            "--json",
        ],
    );
    assert_missing_store_json(
        &world,
        "fast_audit_replay_missing_store_errors_cleanly",
        "lattice audit replay --json",
        &out,
    );
}
