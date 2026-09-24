// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 审计命令错误输出的快速二进制黑盒测试（#40，原 Codeberg #18）。
//!
//! 在项目尚未创建调用账本时，通过已构建的 `lattice` 验证 `audit query`、
//! `replay` 和 `--tail` 返回明确的前提错误及退出码 2。JSON 模式将类型化
//! `bad_request` 记录写到标准输出；文本模式将诊断写到标准错误。
//!
//! 同时保护 `audit::run` 复用顶层异步运行时的行为，避免嵌套创建运行时引发崩溃。
//! 已有账本、真实 Turn 重放和非空查询结果由 `tests/direct_local_turn.rs`
//! 等直接本地场景覆盖；这些场景不依赖已移除的 KVM/MSB 测试通道（#62，原 Codeberg #40）。
//!
//! 本组场景承接原 `planned_audit_cli`，替代 CLI 内部的
//! `audit_query_errors_when_store_missing` 单元测试。历史证据见
//! `docs/archive/records/FINDINGS.md`，现行测试规则见 `docs/development/testing.md`。

use lattice_system_harness::{
    AssertError, CommandOutcome, LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld,
    assert_exit_code, assert_stderr_contains, assert_with_failure_artifacts,
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
    assert_with_failure_artifacts(world, name, summary, out, &[], result)
        .unwrap_or_else(|e| panic!("{e}"));
}

/// 缺少调用账本的公开诊断；文本模式与 JSON 模式共用同一原因。
const MISSING_LEDGER: &str = "invocation ledger not found";

fn require(condition: bool, expected: String) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing { expected })
    }
}

/// 项目根目录和 XDG 状态仓库路径仅供内部定位，公开诊断不回显这些私有路径。
fn no_private_path(world: &ScenarioWorld, out: &CommandOutcome) -> Result<(), AssertError> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for private in [world.project_dir(), world.session_store_dir()] {
        let private = private.display().to_string();
        require(
            !stdout.contains(&private) && !stderr.contains(&private),
            format!("diagnostics must not echo {private}: {}", out.summary()),
        )?;
    }
    Ok(())
}

/// 缺少调用账本时，文本模式将前提错误写到标准错误并返回退出码 2，标准输出为空。
fn assert_missing_store(world: &ScenarioWorld, name: &str, summary: &str, out: &CommandOutcome) {
    let result = assert_exit_code(out, 2)
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

/// `--json` 将同一原因写为标准输出中的类型化错误记录，返回退出码 2，标准错误为空。
fn assert_missing_store_json(
    world: &ScenarioWorld,
    name: &str,
    summary: &str,
    out: &CommandOutcome,
) {
    let result = assert_exit_code(out, 2)
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
                    && record.get("kind").and_then(Value::as_str) == Some("bad_request")
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
    let out = run(
        &world,
        &spec,
        &["audit", "query", "--session", "nobody", "--json"],
    );
    assert_missing_store_json(
        &world,
        "fast-audit-query-missing-json",
        "lattice audit query --json",
        &out,
    );
}

#[test]
fn fast_audit_query_missing_store_errors_cleanly_text() {
    let spec = ScenarioSpec::fast("fast-audit-query-missing-text");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["audit", "query", "--session", "nobody"]);
    assert_missing_store(
        &world,
        "fast-audit-query-missing-text",
        "lattice audit query",
        &out,
    );
}

#[test]
fn fast_audit_tail_missing_store_errors_cleanly() {
    let spec = ScenarioSpec::fast("fast-audit-tail-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(
        &world,
        &spec,
        &["audit", "query", "--session", "nobody", "--tail", "--json"],
    );
    assert_missing_store_json(
        &world,
        "fast-audit-tail-missing",
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
        "fast-audit-replay-missing",
        "lattice audit replay --json",
        &out,
    );
}
