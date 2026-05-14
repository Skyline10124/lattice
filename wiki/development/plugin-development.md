# 插件开发

LATTICE 插件是一个可编排的 LLM 函数：插件负责定义输入如何变成 prompt、LLM 输出如何被解析；`Behavior` 负责决定失败重试、低置信度重试或升级；agent profile 的 `[plugins]` DAG 负责把多个插件串起来。

当前有两种开发路径：

| 路径 | 适合场景 | 是否需要重新编译 |
|------|----------|------------------|
| Declarative manifest plugin | prompt 模板、JSON/text 输出解析、快速迭代、本地热插拔 | 否 |
| Rust typed plugin | 需要强类型 Input/Output、内置工具定义、复杂解析、作为 builtin 发布 | 是 |

优先从 manifest plugin 开始。只有当 manifest 表达不了行为，或者插件需要进入 builtin 分发时，再写 Rust typed plugin。

## Manifest Plugin

目录结构：

```text
examples/plugins/hello/
  plugin.toml
  system.md      # 可选
  template.md    # 可选
  schema.json    # 可选
```

最小 `plugin.toml`：

```toml
[plugin]
name = "local:hello"
description = "Local hot-plug example"
author = "lattice"

[prompt]
system = "You are a local hot-plug demo plugin."
template = "Say hello to {{input}}"

[output]
parse = "text"
```

完整字段：

```toml
[plugin]
name = "local:example"
version = "0.1"
description = "Example local plugin"
author = "local"
preferred_model = "deepseek-v4-flash"

[prompt]
system = "Inline system prompt"
system_file = "system.md"
template = "Task: {{input}}\nContext: {{context}}"
template_file = "template.md"

[output]
parse = "json" # or "text"
schema = { type = "object" }
schema_file = "schema.json"

[behavior]
mode = "strict" # or "yolo"
confidence_threshold = 0.7
max_retries = 3
escalate_to = "human"
```

规则：

- `name` 是全局 plugin id。建议本地插件使用 `local:*` 命名空间。
- `system` 和 `system_file` 二选一。
- `template` 和 `template_file` 二选一；为空时默认使用 `{{context}}`。
- `schema` 和 `schema_file` 二选一。
- file 字段必须是相对路径，不能是绝对路径，不能包含 `..`。
- `output.parse = "json"` 会从模型回复中提取 JSON；`"text"` 会包装成 `{ "content": "..." }`。
- 本地插件可以覆盖同名 builtin；同一个插件目录树里出现重复本地 `name` 会加载失败。

模板变量：

| 变量 | 含义 |
|------|------|
| `{{context}}` | 当前 DAG context 的完整 JSON |
| `{{input}}` | pipeline 初始输入字符串 |
| `{{request}}` | context 中的 `request` 字段；存在时替换 |

## Agent Profile

插件不会单独执行，需要被 agent profile 的 `[plugins]` DAG 引用：

```toml
[agent]
name = "local-hello"
model = "deepseek-v4-flash"
description = "Run a local declarative hot-plug plugin."

[system]
prompt = "Run the configured local plugin and return its output."

[plugins]
entry = "hello"

[[plugins.slots]]
name = "hello"
plugin = "local:hello"
max_turns = 3
behavior = { mode = "yolo" }
```

运行：

```bash
LATTICE_STRICT_CONFIG=0 cargo run -p lattice-cli -- run \
  --pipeline local-hello \
  --agents-dir examples/agents \
  --plugins-dir examples/plugins \
  "Astrin"
```

CLI 会先注册 builtin plugins，再加载 `--plugins-dir` 里的 manifest plugins。`plugin.toml` 可以放在插件目录树的任意子目录中。

## Hot Reload

`--plugins-dir` 存在时，CLI 会启动 watcher，递归监听 `.toml`、`.md`、`.json`、`.txt` 文件。

热加载语义：

- 每次相关文件变化后，loader 重新扫描整个插件目录。
- reload 成功后，`PluginRegistry` 原子替换为新 bundle 集合。
- reload 失败时，旧 registry 保持可用。
- 已经被某个运行中 slot clone 出来的 `PluginBundle` 继续用旧定义；后续 registry lookup 使用新定义。

这意味着热插拔适合长进程或交互式运行时的快速迭代，但不保证正在执行的单个 slot 中途换 prompt。

## Rust Typed Plugin

当 manifest 不够用时，在 `lattice-plugin/src/builtin/` 下添加 Rust plugin。

最小骨架：

```rust
use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Serialize, Deserialize)]
pub struct MyInput {
    pub request: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MyOutput {
    pub result: String,
    pub confidence: f64,
}

pub struct MyPlugin;

impl MyPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for MyPlugin {
    type Input = MyInput;
    type Output = MyOutput;

    fn name(&self) -> &str {
        "MyPlugin"
    }

    fn system_prompt(&self) -> &str {
        "You are a precise LATTICE plugin."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!("Return JSON for this request:\n{}", input.request)
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = crate::builtin::parse_utils::parse_json_from_response(raw)?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
```

然后在 `lattice-plugin/src/builtin/mod.rs` 中：

- `pub mod my_plugin;`
- 在 `builtin_plugin_bundles()` 中追加 `PluginBundle`
- 给新插件补单测，至少覆盖 prompt rendering 和 output parsing

如果插件要暴露给下游 crate，可在 `lattice-plugin/src/lib.rs` 里 re-export。

## DAG 编排

一个 agent profile 可以有多个 plugin slot：

```toml
[plugins]
entry = "plan"
shared_tools = ["read_file", "grep"]

[[plugins.slots]]
name = "plan"
plugin = "PlanGen"
max_turns = 4
behavior = { mode = "strict", confidence_threshold = 0.7, max_retries = 2 }

[[plugins.slots]]
name = "review"
plugin = "CodeReview"
max_turns = 4

[[plugins.edges]]
from = "plan"
rule = { default = true, target = "review" }
```

运行时 context 初始为：

```json
{ "input": "user prompt" }
```

每个 slot 成功后，输出会写入 `context[slot_name]`，后续 slot 可以通过 `{{context}}` 或 typed input 读取上游结果。

## 验证

开发 manifest plugin：

```bash
LATTICE_STRICT_CONFIG=0 cargo run -p lattice-cli -- validate examples/agents
cargo test -p lattice-plugin
```

开发 Rust typed plugin：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo test --workspace --all-targets --no-default-features
```

如果改了示例 profile，也要跑：

```bash
LATTICE_STRICT_CONFIG=0 cargo run -p lattice-cli -- validate examples/agents
```

## 常见选择

- 只是改 prompt 或输出 schema：用 manifest plugin。
- 需要多文件 prompt：manifest 的 `system_file` / `template_file`。
- 需要稳定 JSON 输出：`output.parse = "json"` 加 `schema`，并用 strict behavior。
- 需要新工具定义或复杂解析：写 Rust typed plugin。
- 需要把多个插件组合成流程：写 agent profile 的 `[plugins]` DAG。
