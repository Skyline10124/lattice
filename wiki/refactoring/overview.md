# 重构总览

> Historical note: this table was generated from an older workspace shape. Entries mentioning `lattice-bus`, `lattice-python`, `lattice-agent/src/tools.rs`, or `lattice-agent/src/sandbox.rs` must be re-verified against the current crate map before implementation.

基于 2026-05-06 对全部 6 crate、100+ .rs 文件的逐文件扫描 + 深度二次/三次扫描，以下为所有发现的重构机会汇总。共 77 项（7 P0, 28 P1, 42 P2）。

## 按优先级排序

### P0 — 必须修复（影响正确性或有潜在 bug）

| # | Crate | 文件:行 | 问题 | 建议 |
|---|-------|---------|------|------|
| 1 | bus | profile.rs + lib.rs | `BusConfig::default().max_concurrent_calls=32` vs `BusToml::default().max_concurrent_calls=1`，同一参数不一致默认值 | 统一为合理值（建议 32），TOML 默认应匹配 BusConfig |
| 2 | plugin | orchestration.rs:308-315 | `f64::EPSILON` 用于浮点比较只在值≈1.0 时有效，对 confidence 0.0-1.0 范围偶然可用但不正确 | 用 `(nf-f).abs() < 1e-9` 或直接 `nf == f` |
| 3 | bus | pipeline.rs:130 | 硬编码 `for _turn in 0..20` 循环上限，不可配置 | 改为 `MAX_PIPELINE_ITERATIONS` 常量，通过 BusConfig 或 Pipeline 构造器可配 |
| 4 | bus | lib.rs:329 | `max_concurrent_calls` 同时用作 RPC 并发限制和 channel buffer size，语义混淆 | 分离为两个参数：`max_concurrent_rpc` 和 `channel_buffer_size` |
| 5 | python | engine.rs:304-357 | `stream_chat` 将整个 Stream 收集到 Vec 后再返回 StreamIterator，丧失了流式延迟优势 | 用 crossbeam/tokio channel：async task 推送事件，`__next__` 从 channel 取，真正增量返回 |
| 6 | python | engine.rs:97 | `messages_from_py` 总是传 `None` 给 tool_call_id/name/tool_calls，Python 无法发送 tool-result 或带 tool_calls 的 assistant 消息 | 扩展 Python Message dict 支持 tool_calls、tool_call_id、name 字段 |
| 7 | cli | session.rs:215-236 | `messages_for_agent()` 丢失 tool_calls/reasoning_content/tool_call_id — session 续会时 Agent 收不到之前的 tool call 结果，数据丢失 bug | 扩展 SessionMessage 保存 tool_calls 等，`messages_for_agent()` 正确还原 |

### P1 — 应修复（重复代码、过度复杂、维护负担）

| # | Crate | 文件:行 | 问题 | 建议 |
|---|-------|---------|------|------|
| 7 | plugin | 11 个 builtin | 每个 plugin 的 `parse_output()` 完全相同：`parse_json_from_response` + `serde_json::from_value`，重复 11 次 | 在 Plugin trait 加 default impl（需要 `Self::Output: DeserializeOwned`，已有 trait bound） |
| 8 | plugin | registry.rs | `unwrap_or_else(|e| e.into_inner())` RwLock poison recovery 重复 9 次 | 提取 `write_plugins()` / `read_plugins()` private helper |
| 9 | python | engine.rs:205-236 | `self.router.read().unwrap_or_else(...)` RwLock poison recovery 重复 3 次，仅 log 消息不同 | 提取 `read_router()` helper 方法 |
| 10 | python | lib.rs:8-46 | 10 行几乎相同的 `m.add("...", m.py().get_type::<...>())` 注册样板 | 用 macro `register_exceptions!` |
| 11 | python | errors.rs:21-92 | `convert_core_error` 每个变体都是相同模式，3 处需同步更新（create_exception + convert + register） | 单 macro 生成 exception 定义 + 注册 + convert arm |
| 12 | plugin | lib.rs:326-341 | `PluginRunner::run()` 先序列化 Input 为 Value，然后 `run_plugin_loop` 内 `to_prompt_json` 再反序列化回来 | typed runner 应直接传已格式化的 prompt，消除双序列化 |
| 13 | plugin | lib.rs + erased.rs | `system_prompt_delta()` default impl 在 Plugin trait 和 ErasedPlugin trait 上完全相同 | 删除一处，由 blanket impl 覆盖 |
| 14 | bus | runner.rs:137-181 | `looks_like_error` 44 行 boolean 级联，30+ 条件有重叠 | 改为 regex set 或结构化 matcher |
| 15 | bus | pipeline.rs:460-622 | `run_fork_async` 160 行，内嵌闭包复制了主 run() 的 resolve+assembly+run 逻辑 | 提取 `execute_agent(profile, resolved, input, max_turns)` 共享 helper |
| 16 | bus | runner.rs + micro_agent | Markdown fence stripping 逻辑在两处重复 | 提取到共享 utility 函数 |
| 17 | bus | lib.rs:237-265 | `BusHandler::from_async`、`bus_handler()` free fn、`bus_handler!` macro 三者功能完全相同 | 只保留 free fn + macro |
| 18 | bus | runner.rs:60-81 | JSON schema 验证构建在每次 `run()` 调用时重新计算 | 移到 profile load 阶段一次性验证+缓存 |
| 19 | plugin | dag_runner.rs:236-241 | `ModelRouter::new()` 在每个 slot transition 时重新创建，每次都读 env vars | 构造一次，或由 caller 注入 |
| 20 | plugin | loader.rs:36 | `manifests.sort()` 按 path 排序后立即被 `bundles.sort_by` 按 name 重排 | 删除第一次排序 |
| 21 | bus | pipeline.rs:179-197 | `profile.plugins` 存在但 `pipeline.plugin_registry` 为 None 的检查在每次循环迭代执行 | 移到 Pipeline 构造或 dry_run 一次性检查 |
| 22 | bus | lib.rs:217-221 | `SubscriberEntry.agent_id` 有 `#[allow(dead_code)]`，设置但从未读取 | 移除或实现 audit 功能 |
| 23 | bus | profile.rs:129-139 | `HandoffConfig.handoff_rules` 在 Rust 中但 TOML key 是 `rules`，rename 混淆 | 字段名统一为 `rules`（已在 HandoffConfig 上下文中） |
| 24 | bus | profile.rs:148-237 | 4 个 private TOML 中间体只因 `PluginsConfig`/`PluginSlotConfig` 不支持按 TOML 需要 Deserialize | 在 lattice-plugin 加合适的 Deserialize impl，消除 ~90 行中间体 |
| 25 | core | invocation.rs + sandbox.rs | `is_private_ip()` 在 invocation.rs 和 sandbox.rs 都有实现（router.rs 引用 invocation.rs） | 提取到共享 module（如 `security` 或 `util`） |
| 26 | plugin | lib.rs + dag_runner.rs | timestamp() 逻辑两处重复（Unix epoch microseconds） | 提取到 `parse_utils` 或 `utils` module |
| 27 | plugin | lib.rs:392-424 + dag_runner.rs:206-221 | memory save 逻辑相似但不一致（id 格式不同：`{plugin}-user-{ts}` vs `dag-{slot}-{transitions}`） | 共享 `save_plugin_entry` helper，统一 id scheme |

### P2 — 可选优化（代码质量、性能微调）

| # | Crate | 文件:行 | 问题 | 建议 |
|---|-------|---------|------|------|
| 28 | bus | lib.rs:1-971 | InMemoryBus 实现（480行）+ Bus trait + bus types + tests 全在 lib.rs | 拆到 `bus.rs` module，lib.rs 只保留 type/trait 定义和 re-export |
| 29 | plugin | erased_runner.rs:15-24 | `run_plugin_loop` 8 个参数，调用不可读 | `RunLoopParams` struct + builder pattern |
| 30 | plugin | manifest.rs:125-138 | `render_template` 用 `str::replace`，无 escaping，可能 double-replacement | 单 pass regex 或轻量 template engine |
| 31 | plugin | watcher.rs:78-84 | O(n*m) merge 扫描 builtin+local bundles | HashMap 索引 O(1) lookup |
| 32 | plugin | refactor.rs:5 | RefactorPlugin 依赖 CodeReviewOutput 类型 | review field 用 serde_json::Value 或共享 module |
| 33 | bus | watcher.rs:77 | `PERMITTED_AGENTS_DIR_PARENTS` 硬编码 Linux/macOS 路径 | 用 `dirs` crate 跨平台 |
| 34 | bus | ws.rs:69+87 | `WS_SECRET` env var 在同一 handler 中读取两次 | 缓存第一次结果 |
| 35 | bus | ws.rs:25-30 | `DEFAULT_ALLOWED_ORIGINS` 硬编码 localhost，不可配置 | 从 BusToml 或 env var 可配 |
| 36 | bus | lattice_dir.rs:77-103 | `discover_with_global` 重复 `discover()` 的 bus_toml 路径逻辑 | 提取 private helper |
| 37 | bus | lattice_dir.rs:24-35 | 4 个 `default_*` free fn 给 BusToml serde default | 用 derive Default + explicit 值 |
| 38 | python | pyproject.toml:11 | `requires-python >= 3.9` 但 CI 只测 3.12-3.14 | 收窄到 `>= 3.12` 或加 CI |
| 39 | python | pyproject.toml:14 | MIT + Apache 双 license classifier 不匹配 license=Apache-2.0 | 移除 MIT classifier |
| 40 | plugin | Cargo.toml:17 | `macos_kqueue` feature 全平台启用 | target-specific feature cfg |
| 41 | agent | memory/sqlite.rs | `clone_box()` uses `Box::new(*self)` which panics on OOM; `try_clone()` `.unwrap()` panics if connection reopen fails | Return `Result` from `try_clone()`, handle errors gracefully |
| 42 | agent | memory/sqlite.rs | `save_entry()` uses `.ok()` to discard all errors — silent data loss | At minimum log with `tracing::warn!`, better: return Result |
| 43 | agent | audit.rs | `current_size` field is `#[allow(dead_code)]`, never used for rotation | Remove field or implement size-based rotation |
| 44 | agent | audit.rs | `log_sync()` drops JoinHandle — async write errors silently lost | Store JoinHandle, check on next write or background task |
| 45 | bus | pipeline.rs:51-53 | `Cell<usize>` memory counters not `Sync` — fragile if Pipeline shared across threads | Replace with `AtomicUsize` |
| 46 | core | streaming.rs:21-26 | Hardcoded `MAX_SSE_EVENTS=10000`, `MAX_SSE_DATA_SIZE=1MB`, `MAX_SSE_BUFFER_SIZE=10MB` | Make configurable, add tracing warnings near limits |
| 47 | agent | memory/sqlite.rs | Copy-paste poisoned lock error messages reference wrong method names | Use macro/helper or remove method name from lock recovery |
| 48 | bus | lib.rs:390-424 | `publish()` fire-and-forget — AtMostOnce silently discards errors, AtLeastOnce only warns | Add retry for AtLeastOnce or dead-letter mechanism |
| 49 | core | streaming.rs:46-47 | `StreamEvent::Reasoning` undocumented — which parsers produce it, how it maps to provider formats | Add doc comment specifying parser behavior |
| 50 | cli | app.rs:117 | `ChatMessage.clear_cache()` is `#[allow(dead_code)]` — never called | Remove or use when content updates |
| 51 | cli | app.rs:136-150 | Manual `base64_encode()` for OSC52 clipboard — `base64` crate is already a transitive dep | Use `base64::engine` instead |
| 52 | cli | markdown.rs:24-59 | `has_markdown_syntax()` only scans first 500 chars — may miss syntax in longer messages | Acceptable heuristic, document limitation |
| 53 | bus | profile.rs:78-88+148-190 | YoloSandboxPolicy string-to-enum mapping duplicated in two code paths | Extract `parse_yolo_sandbox_policy()` shared helper |
| 54 | bus | assembly.rs:68-76 | `DefaultToolExecutor::new().expect()` panics 3 times instead of returning Result | Change to Result-returning constructor, propagate error |
| 55 | bus | assembly.rs:53-77 | Tool merging 3 similar branches with duplicated executor construction | Extract `build_tools_and_executor()` helper |
| 56 | plugin | orchestration.rs:306-318 | `values_equal()` Number/String comparison duplicated in two identical arms | Extract `float_equal()` helper |
| 57 | plugin | orchestration.rs:163-164 | `HandoffRule.eval()` any-with-empty-vec silently returns false via `.any()` on empty iterator | Add explicit empty check, document that empty = false |
| 58 | plugin | dag_runner.rs:19 + orchestration.rs:170 | `MAX_DAG_SLOT_TRANSITIONS=50` and `MAX_JSON_DEPTH=50` both hardcoded, different from Pipeline's 20 | Make configurable, standardize limit naming |
| 59 | python | engine.rs:360-372 | `stream_chat` JSON→Python dict conversion via `json.dumps/loads` roundtrip — inefficient | Use PyO3 `ToPyObject` for serde_json::Value directly |
| 60 | cli | commands/run.rs:17-30 | `run()` has 13 parameters with `#[allow(clippy::too_many_arguments)]` | Group into `RunConfig` struct |
| 61 | cli | commands/run.rs:57 | `DefaultToolExecutor::new(".").expect()` — same panic pattern as assembly.rs | Change to Result-returning constructor |
| 62 | cli | session.rs:49-50 | `message_count()` is `#[allow(dead_code)]` — never called | Remove |
| 63 | agent | prompt/compiler.rs:23-27 | `assert_eq!` on sections/budgets length — panic instead of Result | Return `Err(PromptError::MismatchedLengths)` |
| 64 | bus | registry.rs:36-42 | AgentRegistry silently skips failed profile loads with only `tracing::warn!` | Add `failed_loads` field for visibility |
| 65 | cli | commands/run.rs:428-486+488-559 | `EventRenderer::render()` 和 `display_events()` 对 LoopEvent 的处理逻辑几乎完全相同——两个函数都用相同的 match 分支 | 提取 `render_loop_event()` 共享函数，或让 EventRenderer 统一处理两种模式 |
| 66 | cli | commands/run.rs:224-234 | `run_pipeline()` 有 8 个参数，超出可读性阈值 | 同 CLI60，将 pipeline 相关参数归入 `PipelineConfig` struct || 67 | cli | list_agents.rs:26-90 | 手工逐行解析 TOML `[agent]` 段，lattice-bus 已是直接依赖 | 使用 `AgentProfile::load()` 做正式解析 |
| 68 | cli | session.rs:120-123+200-207 | `SessionManager::from_dir()` 和 `latest()` 都有 `#[allow(dead_code)]`，`latest()` 实际在用 | 移除 `latest()` dead_code 标记，`from_dir()` 改用 `#[cfg(test)]` |
| 69 | cli | commands/mod.rs:32 | `safe_agents_dir()` 使用 `std::process::exit(1)` 而非返回 `Result` | 改为返回 `Result<PathBuf, String>` |
| 70 | cli | validate.rs:78-79 | 每个循环迭代创建新 Pipeline 并 `Arc::new(registry.clone())` | 循环外构造 Pipeline，`Arc::new(registry)` 一次 |
| 71 | cli | main.rs:406-427 | `launch_tui()` 8 个参数，与 CLI9/CLI13 同一 pattern | 归入 `TuiOptions` struct |
| 72 | core | errors.rs:220-242 | `scrub_credentials()` 每次调用重新编译 4 个 regex（高频操作） | 用 `LazyLock<Regex>` 或 `RegexSet` 一次性编译 |
| 73 | cli | app.rs:228-266 | App struct 30+ 字段，god object — 状态管理困难 | 拆为 AppInput/AppStream/AppSession 子结构体 |
| 74 | cli | app.rs:965-1255 | `handle_slash_command()` 290 行 match 内联处理 12 命令 | 提取每个命令为独立方法，handle_slash_command 只做 dispatch |
| 75 | cli | app.rs:899-947 | submit() LoopEvent match 是第 4 个副本（强化 CLI12） | 提取统一 `dispatch_loop_event()` 共享函数 |
| 76 | cli | app.rs:747-790 | TUI `!command` bash 模式无沙箱执行，绕过 4 层防御模型 | 通过 DefaultToolExecutor/沙箱执行，或明确文档说明为特权操作 |
| 77 | cli | app.rs:1538 | `trim_tool_result()` 硬编码 LIMIT=4000 不可配 | 改为可配常量或从 Config 读取 |
