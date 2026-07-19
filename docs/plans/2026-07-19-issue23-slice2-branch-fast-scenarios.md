# #23 切片 2：project branch fast 场景 + 删 4 条冗余测试 + 孤儿夹具清理

- 父计划：`2026-07-19-issue23-fast-residual-ledger-closeout.md` §2
- 接管目标（4 条，全部 `lattice-cli`）：
  - `project_branch_creation_json_output_includes_branch`（`session.rs:1284`，经 `SessionApi` 直调）
  - `project_branch_persists_through_reopen`（`session.rs:1345`，经 `SessionApi` 直调）
  - `smoke_session_status_list_shows_seeded_session`（`release_smoke.rs:185`）
  - `smoke_session_status_specific_session`（`release_smoke.rs:210`）

## 已核验契约

- `session new --json` 输出已含 `project_branch` 键（`session.rs:219`），现有
  `assert_session_new_contract` 已断言 `project_branch == "main"`（`fast_session_cli.rs`）——
  **creation JSON 断言实质已被接管**，本切片补的是非默认 branch 值 + status 侧持久化。
- `session status --json` 输出 `project_target_branch`（`session.rs:299`）。
- 内部测试的 reopen = `SessionApi::open_session` 直调；黑盒版用**独立第二次 CLI 进程**读
  status，进程边界严格强于库内 reopen。

## `fast_session_cli.rs` 追加

复用现有 `project_with_agent`/`run`/`ok_or_artifacts`/`parse_json_or_artifacts` 夹具，
不新建文件。保留现有 `create_session` + `assert_session_new_contract`（main 默认契约不弱化），
旁边新增只服务非默认 branch 场景的带参 helper：

```rust
/// Create a session on an explicit project branch and retain the parsed
/// public JSON so the caller can assert branch-specific fields.
fn create_session_on_branch(
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    branch: &str,
) -> (String, Value, Output) {
    let out = run(
        world,
        spec,
        &[
            "--json", "session", "new",
            "--agent", "plain",
            "--project-branch", branch,
        ],
    );
    let parsed = parse_json_or_artifacts(
        world,
        &spec.name,
        "lattice --json session new with explicit project branch",
        &out,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let id = parsed
        .get("session_id")
        .and_then(Value::as_str)
        .expect("session_id")
        .to_string();
    (id, parsed, out)
}

#[test]
fn fast_session_new_json_reports_requested_project_branch() {
    let (world, spec) = project_with_agent("fast-session-new-branch-json");
    let (_id, parsed, out) = create_session_on_branch(&world, &spec, "feature/test");
    let result = assert_exit_success(&out).and_then(|_| {
        if parsed.get("project_branch").and_then(Value::as_str) == Some("feature/test") {
            Ok(())
        } else {
            Err(AssertError::StdoutMissing {
                expected: "project_branch=feature/test in session new output".into(),
            })
        }
    });
    ok_or_artifacts(
        &world,
        &spec.name,
        "session new --project-branch feature/test --json",
        &out,
        result,
    );
}

#[test]
fn fast_session_project_branch_persists_across_cli_invocations() {
    let (world, spec) = project_with_agent("fast-session-branch-reopen");
    let (id, _created, _new_out) = create_session_on_branch(&world, &spec, "feature/persist");

    // Second, independent CLI process: strictly stronger reopen semantics
    // than the in-crate `SessionApi::open_session` round-trip it supersedes.
    let out = run(&world, &spec, &["--json", "session", "status", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let result = assert_exit_success(&out).and_then(|_| {
        let parsed: Value =
            serde_json::from_str(stdout.trim()).map_err(|e| AssertError::StdoutMissing {
                expected: format!("valid JSON (parse error: {e})"),
            })?;
        if parsed.get("project_target_branch").and_then(Value::as_str)
            == Some("feature/persist")
        {
            Ok(())
        } else {
            Err(AssertError::StdoutMissing {
                expected: "project_target_branch=feature/persist after reopen".into(),
            })
        }
    });
    ok_or_artifacts(
        &world,
        &spec.name,
        "session status --json across process boundary",
        &out,
        result,
    );
}
```

> `session status` 已核验为可选位置参数 `session_id`（`SessionAction::Status`），示例 args 可直接执行。
> JSON 键名 `project_branch`（new 输出）与 `project_target_branch`（status 输出）的不对称是现有
> 公开契约，照实断言，不改产品面。

## 删除清单与依据

| 删除 | 接管证据 |
| --- | --- |
| `session.rs:1284` creation JSON | 新场景 1 + 现有 `assert_session_new_contract` |
| `session.rs:1345` reopen 持久化 | 新场景 2（进程边界 reopen） |
| `release_smoke.rs:185` status list | 已落地 `fast_session_new_creates_session_and_appears_in_list`（prose §1.5 判完整接管） |
| `release_smoke.rs:210` status specific | 已落地 `fast_session_status_new_session_shows_zero_commits_and_agent` + `fast_session_status_json_stable_contract` |

同模块**保留**：`project_branch_status_output_includes_target_branch`、
`project_branch_list_output_includes_target_branch`（ledger 判 keep 的局部不变量，不在残留清单）。

## 孤儿夹具清理

删除后检查：`release_smoke.rs` 的 `seed_session` 类 helper、`session.rs`
`project_branch_tests` 模块的 `temp_api` 是否仍有 caller；孤儿即删，模块空则整删。
**禁止**为保留测试新增 shared helper（票文红线）。

## ledger 更新

4 条 reason 改「已删除原内部测试，由已落地 fast_session_cli（…）黑盒接管」，
去掉「冗余保留待删」字样；身份字段不动。

## 验收

- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness --test fast_session_cli` 全绿。
- `cargo test -p lattice-cli` 全绿（删除后无编译残留）。
- `cargo xtask ci` + `harness-boundary` 通过。
