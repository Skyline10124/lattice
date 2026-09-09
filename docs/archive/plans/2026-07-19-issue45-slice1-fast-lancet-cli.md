# #45 切片 1：`fast_lancet_cli` 场景 + 删除内部空态测试

- 父计划：`2026-07-19-issue45-fast-residual-ledger-closeout.md` §1
- 接管目标：`planned_lancet_cli`（1 条）——`cep_status_missing_db_returns_empty_state`
  （`crates/lattice-cli/src/commands/lancet.rs:289`）

## 已核验契约

- `lattice lancet status --json` 以 **cwd 为 project root**（`lancet.rs:46`），
  audit DB 缺失时 exit 0，JSON 恰含两键：`recent_calls`（数组）+ `last_guard_verdict`
  （`lancet.rs:58-69` `status_json_value`）。
- text 模式空态输出含 `Recent CEP Calls:`、`none` 与 `Last Guard Verdict`（`status_text_lines`）。
- harness 驱动模式照抄 `fast_audit_cli.rs`（同为"无 turn → store 缺失"家族）。

## 新文件 `crates/lattice-system-harness/tests/fast_lancet_cli.rs`

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fast binary black-box: `lattice lancet status` empty-state surface
//! (triage replacement target `planned_lancet_cli`).
//!
//! With no agent turn having run, the project-keyed audit store does not
//! exist; `lancet status` must report an empty state with exit 0 (not an
//! error, unlike `audit query` which is an explicit query against a missing
//! store). Supersedes the crate-internal
//! `cep_status_missing_db_returns_empty_state` unit test in `lattice-cli`.

use std::process::Output;

use lattice_system_harness::{
    AssertError, LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld,
    assert_exit_success, assert_stdout_contains, assert_with_failure_artifacts,
};
use serde_json::Value;

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

fn run(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Output {
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
    out: &Output,
    result: Result<(), AssertError>,
) {
    assert_with_failure_artifacts(world, name, summary, out, &[], result)
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn fast_lancet_status_missing_audit_store_reports_empty_state_json() {
    let spec = ScenarioSpec::fast("fast-lancet-status-empty-json");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["lancet", "status", "--json"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let result = assert_exit_success(&out).and_then(|_| {
        let parsed: Value =
            serde_json::from_str(stdout.trim()).map_err(|e| AssertError::StdoutMissing {
                expected: format!("valid JSON (parse error: {e})"),
            })?;
        let empty_calls = parsed
            .get("recent_calls")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        let null_verdict = parsed
            .get("last_guard_verdict")
            .is_some_and(Value::is_null);
        if empty_calls && null_verdict {
            Ok(())
        } else {
            Err(AssertError::StdoutMissing {
                expected: "recent_calls=[] and last_guard_verdict=null".into(),
            })
        }
    });
    ok_or_artifacts(
        &world,
        "fast-lancet-status-empty-json",
        "lattice lancet status --json (no audit store)",
        &out,
        result,
    );
}

#[test]
fn fast_lancet_status_missing_audit_store_text_mode_smoke() {
    let spec = ScenarioSpec::fast("fast-lancet-status-empty-text");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["lancet", "status"]);
    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "Recent CEP Calls:"))
        .and_then(|_| assert_stdout_contains(&out, "none"))
        .and_then(|_| assert_stdout_contains(&out, "Last Guard Verdict"));
    ok_or_artifacts(
        &world,
        "fast-lancet-status-empty-text",
        "lattice lancet status (no audit store, text)",
        &out,
        result,
    );
}
```

## 删除

场景通过后删 `lancet.rs` `#[cfg(test)]` 内的 `cep_status_missing_db_returns_empty_state`
（`:289`）。同模块 `cep_status_seeded_audit_json` 是 seeded 态（属 `planned_audit_ops_*`
延期账），**保留**。若 `ScopedXdgStateHome`/`seed_audit_db` 夹具仍被 seeded 测试使用则不动。

## ledger 更新

该条 reason 改：「已删除原内部测试，由已落地 fast_lancet_cli
（lancet status 空态 JSON/text）黑盒接管」；`decision`/`target_layer`/身份字段不动。

## 验收

- 两场景在 `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness --test fast_lancet_cli` 通过。
- `cargo xtask ci` 全绿；harness 无新增产品 crate 依赖。
