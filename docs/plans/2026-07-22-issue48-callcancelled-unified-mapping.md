# #48:cooperative Component cancellation 统一映射为 `CallCancelled`

- 来源:Codeberg issue #48(#5 Spec 审查 follow-up,severity High,`ready-for-agent`)。
- 前置核验:issue 最新 comment(2026-07-22)列出的两处缺口已逐行复核属实;另发现
  audit `Cancelled` 臂当前是 dead code —— 唯一产出 `"call cancelled"` message 的
  `call_cancelled()` 只在 dispatch 客户端使用,服务端永远不会返回它,因此连现已正确
  映射的 epoch cancellation 今天也被记成 `Error`。
- 取舍(已在 issue 确认):
  1. runtime 以 call-scoped cancellation state 为唯一分类依据,统一映射
     `CallCancelled`(不认 guest 错误字符串);
  2. transport `finalize_audit_for_dispatch` / `is_incomplete_dispatch` 改认
     `CepErrorCode::CallCancelled`;
  3. dispatch **不**升级 `call_cancelled()` 的 code,改为客户端 cancel 后等待服务端
     结构化响应再返回(服务端是 call outcome 的唯一 authority),并删除
     `call_cancelled()`。
- 完成态:同一 call-scoped cancellation 无论从哪个观察点命中(host pre-check /
  guest cooperative probe / epoch interruption),客户端错误码与 audit outcome 都收敛
  为一致结果;非 cancellation 的 guest logical error 语义不变。

## 48.1 `lancet-runtime`:cancellation state 为唯一分类依据

`crates/lancet-runtime/src/error_map.rs` 新增共享 helper,`AbortReason::Cancelled` 臂
改调它,消除重复构造:

```rust
/// Structured cancellation error. Call-scoped cancellation state is the
/// sole authority; the guest's error string plays no role.
pub(crate) fn cancelled_error(method: &str) -> CepError {
    CepError::tool_execution(
        CepErrorCode::CallCancelled,
        method,
        "cancelled",
        None,
        Retriability::Permanent,
    )
}
```

`crates/lancet-runtime/src/runtime.rs`(`use crate::error_map::cancelled_error;`):

1. params validation 之后、`self.new_store(ctx)` 之前(`ctx` 尚未 move):

```rust
// Pre-cancelled call: never enter Wasmtime.
if ctx.cancellation.is_cancelled() {
    return Err(cancelled_error(name));
}
```

1. guest logical error 路径(现有 `let raw = match raw { ... }` 块)替换为:

```rust
let raw = match raw {
    Ok(output) => output,
    Err(_) if store.data().call.cancellation.is_cancelled() => {
        return Err(cancelled_error(name));
    }
    Err(guest_err) => return Err(guest_error(name, guest_err)),
};
```

为便于确定性单测,第 2 点抽成纯函数(in-crate,48.4-2 直接测它):

```rust
fn map_guest_logical_result(
    name: &str,
    raw: Result<ToolOutput, String>,
    cancelled: bool,
) -> Result<ToolOutput, CepError> {
    match raw {
        Ok(output) => Ok(output),
        Err(_) if cancelled => Err(cancelled_error(name)),
        Err(guest_err) => Err(guest_error(name, guest_err)),
    }
}
```

## 48.2 `lancet-native` transport:audit 认 code,清理 dead 臂

`crates/lancet-native/src/server/transport.rs`:

```rust
fn is_incomplete_dispatch(result: &Result<Value, CepError>) -> bool {
    matches!(result, Err(err) if err.code == CepErrorCode::CallCancelled)
}
```

`finalize_audit_for_dispatch` 的取消臂改为按 code 匹配,并删除旧的
`err.message == "call cancelled"` 臂(其唯一生产者是客户端 `call_cancelled()`,
服务端不可达,dead code):

```rust
Err(err) if err.code == CepErrorCode::CallCancelled => {
    let _ = audit.finalize_call(audit_start, AuditOutcome::Cancelled, None, None);
}
```

`is_incomplete_dispatch` 不再保留 `ExecutionFailed + message.contains("cancelled")`
字符串子句:与「不以 guest-controlled 字符串分类」一致;修好后所有真实 call-scoped
cancellation 都是 `CallCancelled`,partial-effects durable metadata 路径不回退(今天
cooperative guest cancel 恰好能命中字符串子句,不扩展 code 匹配反而会回退它)。

## 48.3 `lancet-dispatch`:客户端等待服务端结构化响应

`crates/lancet-dispatch/src/transport.rs` `start_call_raw` 的 select 改为:

```rust
let mut response_rx = response_rx;
let response = tokio::select! {
    res = &mut response_rx => {
        guard.complete();
        res.map_err(|_| CepError::internal_error("response channel closed"))??
    }
    _ = wait_cancelled(&mut cancel_rx) => {
        // Cancellation requested: the server observes the same watch and
        // maps the outcome authoritatively. Await its response instead of
        // synthesizing a client-side error.
        response_rx
            .await
            .map_err(|_| CepError::internal_error("response channel closed"))??
    }
};
```

配套改动:

- 删除 `registry.rs` 的 `call_cancelled()`、`lib.rs` 与 `lancet-cep` 的 re-export
  (全 workspace 仅客户端 select 臂一个调用点);`transport.rs` 的 import 同步清理。
- `start_call_raw` 提升为 `#[doc(hidden)] pub`(与 `start_call`/`call_raw` 同级):
  round-trip 测试需持有 `CallControl` 以断言客户端可见错误码。
- `start_call` / `CallControl` doc comment 更新:cancel 后 pending future 以服务端
  结构化 outcome resolve,不再立即返回客户端合成错误;「drop future = 发 cancel」
  语义不变。
- 有界性:Component 路径由 epoch ticker(5ms)+ deadline 保证服务端必定响应;
  native handler 不可抢占,cancel 后客户端等待至 handler 完成(见决策 D2)。

## 48.4 测试计划(TDD 顺序)

1. `crates/lancet-runtime/tests/call.rs`:`call_with_precancelled_returns_guest_error`
   改名 `precancelled_call_returns_call_cancelled`,断言改
   `CepErrorCode::CallCancelled`(issue 回归测试示意原样适用)。
2. `crates/lancet-runtime/src/runtime.rs` in-crate 单测 `map_guest_logical_result`:
   - guest `Err` + cancelled=true → `CallCancelled`;
   - guest `Err` + cancelled=false → `ExecutionFailed`(非 cancellation 语义不变);
   - `Ok` 原样透传。
3. `crates/lancet-dispatch/src/transport.rs` 单测:
   - handler 观察 cancel 后返回结构化错误 → 客户端收到**服务端那个错误**
     (code/message/data 原样透传),不再是客户端合成的 `ExecutionFailed`;
   - drop future 仍经 `CancelGuard` 向服务端发 cancel(handler 内 `Cancellation`
     可观察);
   - 现有 round-trip / invalid-params / deserialize 用例不变。
4. `crates/lancet-native/tests/cep_component_round_trip.rs`:
   - 改写 `cancelled_component_call_server_survives`:经 `start_call_raw` 持有
     `CallControl`,立即 `cancel()` 再 await —— 断言客户端 `CallCancelled`、audit
     `result_or_error == "cancelled"`、后续调用成功(服务端存活)。cancel 先于
     await,服务端 pre-check 必然命中,用例确定无竞态;
   - 修正该测试 doc comment(现文声称 audit `Cancelled` 是「handler 未完成的
     transport 取消」专用 outcome,事实上该臂不可达,属错误留痕);
   - 保留一个 timeout-drop 变体:drop 后 audit 仍记 cancelled、server 存活。

## 决策记录

- D1 **分类 authority**:call-scoped `Cancellation` watch 是唯一依据,guest 错误
  字符串不参与分类(issue 验收明确)。副作用:audit message 臂与
  `is_incomplete_dispatch` 字符串子句一并去除。
- D2 **客户端等待服务端**:cancel 后 future 等服务端响应。Component 有界(epoch
  tick 5ms + deadline);native handler 不可抢占,cancel 后需等其跑完 —— 现有 5 个
  native 方法均为有界 workspace 操作,可接受。drop-future 语义不变(不等任何东西)。
- D3 **收敛性论据**:host pre-check、guest probe、epoch 三个观察点输出同一个
  `CallCancelled`,先后竞态对外不可见 —— 这是本修复的核心正确性性质。mid-run
  cooperative 路径无法用 instant noop guest 做确定性集成测试(host pre-check 与
  guest probe 之间是纳秒级窗口;不为测试给 guest 加自旋等待),由 48.4-2 纯函数
  单测 + 本收敛性论据覆盖。
- D4 **固有竞态窗口(记录不处理)**:cancel 已置位但 guest 因其他原因先 trap → 仍
  映射 `ToolTrap`;guest 先返回 `Ok` 后 cancel 才生效 → 返回成功。两者均为「取消在
  操作完成后才被观察」的固有语义;code-based audit 后不破坏一致性。
- D5 **`call_cancelled()` 删除**:唯一调用点是客户端 select 臂;该 pub API 及
  `lancet-cep` re-export 一并移除(workspace 内部,无外部消费者)。

## Out of scope

- 非 cancellation 的 guest error 公开语义(`ExecutionFailed` / `guest_error` 形状不变)。
- guest 自报可信 cancellation reason。
- native handler 可抢占取消(无此机制,也不引入)。
- async wasmtime 迁移(FINDINGS.md M-TOOL-02 已跟踪)。
- transport lifecycle 其他改动(server shutdown、event 顺序等不变)。

## 验证

```bash
cargo +1.96.0 test -p lancet-runtime
cargo +1.96.0 test -p lancet-dispatch
cargo +1.96.0 test -p lancet-native --test cep_component_round_trip
cargo +1.96.0 clippy -p lancet-runtime -p lancet-dispatch -p lancet-native --all-targets -- -D warnings
```

`lancet-dispatch/AGENTS.md` 要求 transport/cancellation 变更同步跑 native 与
lattice-api round-trip 测试,一并执行:

```bash
cargo +1.96.0 test -p lancet-native
cargo +1.96.0 test -p lattice-api
```

落地后按仓库惯例收尾:追加 `FIX.md` 条目(issue id/severity、问题、修复摘要、
验证命令与结果、变更文件);更新 `crates/{lancet-runtime,lancet-dispatch,lancet-native}/AGENTS.md`
的 Recent Issues / Next Actions。
