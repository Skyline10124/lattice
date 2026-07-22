# #23 切片 4：TEST-TRIAGE prose ↔ machine ledger 原子同步

- 状态：已完成（2026-07-19；ledger/prose 已落地，见 FIX.md #23 slice 3/4）
- 父计划：`2026-07-19-issue23-fast-residual-ledger-closeout.md` §4
- 前置：切片 3 已在同一 PR 的前一 commit 改完 JSON；本切片不得改变测试身份或新增场景实现。
- 目标：`docs/TEST-TRIAGE.md` 成为 JSON 判决书的可读、可复核投影。

## 4.1 从 JSON 重算唯一计数

```python
import collections, json
from pathlib import Path

entries = json.loads(Path("docs/test-triage-decisions.json").read_text())
assert len(entries) == 1449

decisions = collections.Counter(e["decision"] for e in entries)
layers = collections.Counter(e["target_layer"] for e in entries)
by_crate = collections.defaultdict(collections.Counter)
replacements = collections.Counter()
for e in entries:
    by_crate[e["crate"]][e["decision"]] += 1
    raw = (e.get("replacement") or "").strip()
    if raw.startswith("n/a"):
        continue
    for atom in (part.strip() for part in raw.split("|")):
        if atom:
            replacements[atom] += 1

print("decisions", decisions)
print("layers", layers)
for crate, c in sorted(by_crate.items()):
    print(crate, c["keep"], c["elevate"], c["delete"])
print("replacement atoms", replacements)
```

输出粘到 PR 描述，作为 prose 修改依据；禁止从旧 Markdown 反推数字。

## 4.2 更新 `TEST-TRIAGE.md` §1 场景目录

新增/替换示例：

```markdown
| 场景 ID | Layer | 状态/归属 |
| `direct_local_plain_text_turn_commits_and_exit_zero` | `direct_local_binary` | #40 已落地 |
| `planned_direct_message_assembly` | `direct_local_binary` | #24 精确断言 message order/content |
| `planned_direct_tool_error_payload` | `direct_local_binary` | #24 精确断言 CEP payload |
| `planned_direct_audit_query_seeded` | `direct_local_binary` | #24 query seeded DB |
| `planned_direct_audit_tail_seeded` | `direct_local_binary` | #24 tail seeded DB |
| `planned_direct_branch_create_isolation` | `direct_local_binary` | #24 branch-create cross-session isolation |
```

目录必须逐行登记切片 3 §3.4 的 **44 个 exact ID**（不能只登记 family），并覆盖 JSON
`replacement` 的每个 normalized atom；退役 KVM-lane layer / planned ID 不留作 active alias。
历史说明可保留文本“此前名为 …”，但不得再出现在机器字段或 active 场景表。

## 4.3 重写 §1.5 执行状态

结构固定为：

```markdown
## 1.5 #23 最终执行状态（2026-07-19）

### Fast residual：0
- 切片 1：`fast_lancet_cli` 接管 1 条并删除原内部测试。
- 切片 2：branch create/reopen 接管 2 条；另删 2 条已接管 smoke。

### #40 骨架已完成接管
| replacement scenario | 删除的内部测试 | superset 依据 |

### 精确移交 #24（44 行）
| exact planned scenario ID | ledger identity | assertion bundle |
| ...从切片 3 §3.4 逐行投影，不按大类聚合... | ... | ... |

### MSB 词汇退役
#40 删除 Upper / KVM lane；active ledger 只使用 `direct_local_binary` / `*_direct`。
```

旧的「退役 lane 项需 KVM」表保留为历史时必须明确标 obsolete，不得与当前状态并列成 active 延期表。

## 4.4 crate 小节计数校验（可执行示例）

```python
import collections, json, pathlib

entries = json.load(open("docs/test-triage-decisions.json"))
by = collections.defaultdict(collections.Counter)
for e in entries:
    by[e["crate"]][e["decision"]] += 1

prose = pathlib.Path("docs/TEST-TRIAGE.md").read_text()
missing = []
for crate, count in sorted(by.items()):
    expected = (
        f"### `{crate}`（保留 {count['keep']} / "
        f"提升 {count['elevate']} / 删除 {count['delete']}）"
    )
    if expected not in prose:
        missing.append(expected)
if missing:
    raise SystemExit("crate count headers out of sync:\n" + "\n".join(missing))
```

## 4.5 replacement 覆盖校验（防未登记 atom）

```python
import json, re
from pathlib import Path

entries = json.loads(Path("docs/test-triage-decisions.json").read_text())
atoms = set()
for e in entries:
    raw = (e.get("replacement") or "").strip()
    if not raw or raw.startswith("n/a"):
        continue
    for part in raw.split("|"):
        atom = part.strip()
        if atom:
            atoms.add(atom)
prose = Path("docs/TEST-TRIAGE.md").read_text()
missing = sorted(atom for atom in atoms if f"`{atom}`" not in prose)
assert not missing, f"replacement scenarios absent from prose catalog: {missing}"
# Active ledger must not retain retired KVM-lane layer / planned tokens.
assert not any(
    e.get("target_layer", "").startswith("real_") and "sand" in e.get("target_layer", "")
    for e in entries
)
assert not any(re.search(r"(?:^|_)msb(?:_|$)", atom) for atom in atoms)
```

`n/a` 与 `n/a — ...` 均由 `startswith("n/a")` 明确分类为非场景 sentinel；compound atom
先 `strip()`，空 atom 拒绝/忽略，不让空白制造重复 ID。

## 4.6 FIX.md（落地后才追加）

遵循根 AGENTS：只在切片 1–4 全部 landing 且门禁通过后追加，不提前写。模板：

```markdown
## #23 — fast residual 与 Direct-Local triage ledger 收口

- Severity/ID: testing / #23
- Problem: 5 条 fast residual；57 条 active 退役 KVM-lane layer（55 elevate + 2 keep）。
- Fix: external-binary takeover；11 条退役契约补记 delete；其余逐条映射 #40 场景或 44 个 assertion-level #24 ID。
- Verification: <实际命令与通过结果>
- Changed files: <实际清单>
```

## 4.7 验收

- 上述三个脚本（计数、crate headers、normalized replacement catalog）均 exit 0；
  当时基线 decision 计数为 keep 1337 / elevate 86 / delete 26（若执行前源变化则按切片 3 重新核验）。
- Markdown 与 JSON 总数、decision、target layer、replacement、最终状态一致。
- active JSON / prose 场景表不得再出现退役 KVM-lane layer、带退役 lane 后缀的 planned ID、或全 turn 退役 scenario 名；仅允许明确标识为历史/obsolete 的 prose。
- `cargo xtask ci`、`cargo xtask harness-boundary`、`cargo xtask lancet-boundary` 全绿。
- 与切片 3 一起 review/merge；账本中间态不落主干。
