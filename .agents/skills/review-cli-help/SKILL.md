---
name: review-cli-help
description: >-
  审查 `lattice` 与 `lattice-daemon` 全部子命令的 `--help` / `-h` 帮助文本与命令行参数错误提示。
  在新增或修改 CLI 子命令、选项、占位符、文案，或用户要求统一检查所有帮助信息时使用。
---

# 审查 CLI 帮助信息

本技能用于系统审查 `lattice`（含全部子命令）与 `lattice-daemon` 的帮助输出及参数错误提示，确保全命令树符合统一中文规范、与实现及合同一致。

## 1. 读取规范基准

审查前先读取以下现行规则源：

- CLI 帮助与参数错误规则：[crates/lattice-cli/AGENTS.md](../../../crates/lattice-cli/AGENTS.md#帮助与参数错误)
- 后台二进制约束：[crates/lattice-daemon/AGENTS.md](../../../crates/lattice-daemon/AGENTS.md)
- 领域术语表：[docs/reference/glossary.md](../../../docs/reference/glossary.md)

若改动涉及带有明确文案要求的架构合同（例如 [docs/architecture/llm/usage.md](../../../docs/architecture/llm/usage.md#费用估算) 对费用未知显示 `-` 的要求），一并读取对应条款。

## 2. 构建并导出全量帮助

先构建最新的 `lattice` 与 `lattice-daemon` 二进制（低磁盘开销）：

```bash
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo build -p lattice-cli -p lattice-daemon --bins
```

再执行遍历与预检脚本 [collect_help.py](./scripts/collect_help.py)：

```bash
python3 .agents/skills/review-cli-help/scripts/collect_help.py
```

脚本会自动递归发现 `lattice` 的全部子命令（不会遗漏新增命令）并连同 `lattice-daemon` 一起执行以下机械检查，然后输出每条命令的完整 `--help` 正文与问题汇总：

- `--help` 与 `-h` 均以退出码 `0` 返回、无 `stderr`，且两者输出逐字节一致
- 无 clap 英文残留（`Usage:`、`Commands:`、`Arguments:`、`Options:`、`Print help`、`[default:`、`[possible values:` 等）与 `help` 子命令泄漏
- 用法行 `用法：...` 与当前命令路径一致，且不含英文 `[OPTIONS]` / `<COMMAND>`
- 参数与选项签名中无全大写英文自动占位符（如 `<SESSION_ID>`）或带空格占位符
- 描述条目末尾不带中文句号 `。`
- 全局选项（`-j`、`-v`、`-h`、`-V`）固定沉底且顺序一致
- 输出全命令树的 `--json` 暴露矩阵与去重占位符汇总，便于核对跨命令一致性
- 标注包含括号的描述行，供人工复核是否属于多余补充

仅看检查结果不打印全文时可加 `--check-only`；二进制不在默认路径时可用 `--lattice-bin` / `--daemon-bin` 或 `LATTICE_BIN` / `LATTICE_DAEMON_BIN` 指定。

## 3. 逐项人工审查

逐条阅读脚本导出的帮助正文，对照源码核查脚本无法判定的语义项：

| 维度 | 审查重点 | 对照源码 |
| --- | --- | --- |
| 措辞与精简度 | 摘要与选项说明是否直白简短、无多余括号或重复解释；是否误把临时实现限制（如当前仅支持某 provider）写进通用帮助 | [main.rs](../../../crates/lattice-cli/src/main.rs)、`crates/lattice-cli/src/commands/`、[lattice-daemon/src/main.rs](../../../crates/lattice-daemon/src/main.rs) |
| 术语与占位符 | 同类概念跨命令是否共用同一占位符（如 `<会话ID>`、`<路径>`、`<节点号>`、`<渠道>`、`<Agent>`、`<provider>`）；是否符合[领域术语表](../../../docs/reference/glossary.md)并避开 `_Avoid_` 词 | 全量 `--help` 输出 |
| `--json` 一致性 | 每个命令的 `--json` 描述（JSON / JSONL 流 / 混合）是否与实际输出一致；不支持 `--json` 的命令是否在帮助中隐藏 `-j, --json` 并在解析阶段以退出码 `2` 拒绝 | [help.rs](../../../crates/lattice-cli/src/help.rs)、[main.rs](../../../crates/lattice-cli/src/main.rs) |
| 注册表完整性 | `usage_for_path`、`json_support_for_path`、`help_hint_for_args` 是否覆盖全部子命令且无过时条目；自定义值解析器的中文错误前缀是否已加入 `safe_validation_reason` | [help.rs](../../../crates/lattice-cli/src/help.rs) |
| 合同与错误脱敏 | 合同规定的说明（如 `session usage` 注明“等量 API 费用”与“未知项显示 -”）是否齐全；新增或改动命令在缺参、未知参数、非法值、冲突参数与 `--json`（含伪密钥）下是否均输出脱敏中文、指向最深层 `--help` 并退出 `2` | [help.rs](../../../crates/lattice-cli/src/help.rs)、[fast_cli_help.rs](../../../crates/lattice-system-harness/tests/fast_cli_help.rs) |

## 4. 输出报告与验证

审查结论按三类分组汇报，每条写明命令路径、当前原文与修改建议：

1. **必须修复**：违反 [帮助与参数错误](../../../crates/lattice-cli/AGENTS.md#帮助与参数错误)、英文/占位符/错误脱敏缺陷或与实现/合同不一致。
2. **建议精简**：多余括号、过长措辞或跨命令表述不统一。
3. **核验范围**：本次遍历到的命令总数、自动化预检结果以及是否已抽检参数错误。

若在本次任务中一并完成修复，重新构建后运行验证并再次执行 `collect_help.py` 确认清零：

```bash
umask 0077 && TMPDIR=/tmp cargo test -p lattice-cli -p lattice-daemon
TMPDIR=/tmp cargo test -p lattice-system-harness --test fast_cli_help
python3 .agents/skills/review-cli-help/scripts/collect_help.py --check-only
```
