# Nix 范式

## LLM = Builder

```nix
derivation {
  name = "code-review";
  builder = llm("sonnet");        # LLM = builder
  args = [review_prompt, diff];   # 输入严格受控
  output = [review_result.json];  # 输出可验证
}
```

代码决定构建图，LLM 只执行推理。Nix 从来不问 builder 要不要 build。

## 声明式配置

```
artemis.toml     # model = "sonnet", budget = "$50"
artemis.lock     # provider=anthropic, model=claude-sonnet-4-6-20250514
```

## 内容寻址缓存

```
sha256(prompt + model + params) → response
```
