# 插件系统设想

## 插件结构

```
lattice-code-review/
  setup.py
  prompts/review.md      # 自然语言：agent 身份 + 推理策略
  src/
    input.py             # ReviewInput + to_prompt()
    output.py            # ReviewOutput + from_output() + validate()
    behavior.py          # 代码控制：handoff, retry, 超时
```

## 类型化插件

```python
class CodeReviewPlugin(Plugin):
    def build_input(self, ctx): ...
    def to_prompt(self, input): ...
    def from_output(self, raw): ...
    def should_handoff(self, output) -> Optional[AgentId]: ...
```

## 组合

```
code-review + security-audit → 安全审查 agent
refactor + test-gen → TDD agent
```

## 行为模式

```python
class YoloBehavior:
    """LLM 自主决定一切"""
    def should_handoff(self, output): return output.suggested_handoff

class StrictBehavior:
    """代码控制，确定性"""
    def should_handoff(self, output):
        if output.confidence < 0.7: return None
        return output.suggested_handoff
```

同一插件，不同 behavior。YOLO 或不 YOLO，只是 behavior 松紧不同。类型边界始终生效。
