# lattice-core 重构建议

## 已发现的重构机会

### C1 — is_private_ip() 在两处重复实现

**文件**:
- `src/invocation.rs:21-53` — `is_private_ip(host: &str) -> bool`
- `src/router.rs:786-819` — `is_private_or_reserved(ip: IpAddr) -> bool`

**问题**: 相同功能（判断 IP 是否为私有/链路本地地址）在两个文件中都有实现。`is_private_or_reserved` 是扩展版（覆盖更多 reserved range），`is_private_ip` 是简化版（只做 DNS resolve + private check）。

**建议**: 
- 提取到 `security.rs` 或 `util.rs` module
- `is_private_ip(host: &str)` → resolve + `is_private_or_reserved`
- `is_private_or_reserved(ip: IpAddr)` → 独立函数

### C2 — invocation.rs 的 import 重复

**文件**: `src/invocation.rs` imports

**问题**: 有 `use std::env;` 和 `use std::sync::{LazyLock, Mutex};` 重复 import（在 line 和 test_support section）。

**建议**: 合并 import section。

### C3 — router.rs 的 import 重复

**文件**: `src/router.rs` imports

**问题**: 有两处 `use std::env;` 和 `use std::sync::{LazyLock, Mutex};`。

**建议**: 合并 import section。

### C4 — streaming.rs 1593 行，占比过高

**文件**: `src/streaming.rs` (1593行)

**问题**: streaming.rs 占整个 crate 的 18%。包含：
- 3 个 SSE parser（OpenAI 110行, Anthropic 120行, Gemini 165行）
- Raw SSE parser (80行)
- Stream conversion (110行)
- 40+ 个 test 函数 (600+ 行)

**建议**: 
- 拆分 parser 到 `streaming/openai.rs`, `streaming/anthropic.rs`, `streaming/gemini.rs`
- 拆分 test 到 `streaming/tests.rs`
- `streaming/mod.rs` 只保留 trait 定义 + sse_from_bytes_stream + parse_raw_sse

### C5 — catalog/types.rs ApiProtocol 自定义 serde 冗长

**文件**: `src/catalog/types.rs:26-76`

**问题**: ApiProtocol 有 50 行自定义 Serialize/Deserialize/FromStr 实现，主要处理短别名映射。可以用 serde rename + helper 更简洁。

**建议**: 
```rust
#[serde(rename_all = "snake_case")]
pub enum ApiProtocol {
    #[serde(rename = "openai", alias = "openai_chat", alias = "chat_completions")]
    OpenAiChat,
    ...
}
```
如果 serde `alias` feature 不够用，至少用 macro 简化实现。

### C6 — errors.rs 测试密度过高

**文件**: `src/errors.rs` (720行)

**问题**: 420+ 行测试函数（占 58%），只有 300 行生产代码。

**建议**: 拆分测试到 `errors/tests.rs` 模块。生产代码保留在 errors.rs。

### C7 — Transport trait default methods 可提取到 TransportBase

**文件**: `src/transport/mod.rs:139-293`

**问题**: Transport trait 有 9 个 default method（normalize_messages, normalize_tools, denormalize_stream_chunk 等），大部分依赖 self.base_url() 和 self.extra_headers()。`TransportBase` 已有这些字段。

**建议**: 将 default impl 移到 `TransportBase` 提供的方法中，Transport trait 只定义必须 override 的方法（normalize_request, denormalize_response, chat_endpoint, auth_header_name, auth_header_value, create_sse_parser）。

### C8 — SSE streaming hard limits not configurable

**文件**: `src/streaming.rs:21-26`

**问题**: `MAX_SSE_EVENTS=10000`, `MAX_SSE_DATA_SIZE=1_000_000` (1MB), `MAX_SSE_BUFFER_SIZE=10_000_000` (10MB) are hardcoded constants. Long streaming responses (e.g. large code generation) could hit these limits silently.

**建议**: Make these configurable via `StreamingConfig` or transport-level parameters. At minimum, add tracing warnings when approaching limits rather than silently truncating.

### C9 — streaming.rs StreamEvent::Reasoning variant has no parser documentation

**文件**: `src/streaming.rs:46-47`

**问题**: `StreamEvent::Reasoning { content: String }` was added for DeepSeek R1/V4 thinking chains, but there's no documentation about which SSE parsers produce this variant and how it maps to provider-specific streaming formats.

**建议**: Add doc comment specifying that AnthropicSseParser emits Reasoning events from `thinking` content blocks, and document Gemini/non-DeepSeek provider behavior (likely no Reasoning events).
### C10 — scrub_credentials() 每次调用重新编译 4 个 regex

**文件**: `src/errors.rs:220-242`

**问题**: `scrub_credentials()` 在每次调用时用 `regex::Regex::new(pattern)` 编译 4 个正则模式。错误响应脱敏是高频操作（每个错误响应都调用），regex 编译开销不必要。

**建议**: 用 `LazyLock<Regex>` 或 `regex::RegexSet` 一次性编译，scrub_credentials() 直接使用预编译的 regex。
