# 模型解析

## 流程

```
"sonnet"
  → normalize_model_id → "sonnet" (已是标准格式)
  → resolve_alias → "claude-sonnet-4-6"
  → catalog.get_model → ModelCatalogEntry
  → 遍历 providers (按 priority 排序)
  → resolve_credentials (检查环境变量)
  → 返回 ResolvedModel
```

## 归一化

```
"ANTHROPIC/claude-sonnet-4.6" → "claude-sonnet-4-6"  (strip OpenRouter prefix)
"us.anthropic.claude-sonnet-4-6-v1:0" → "claude-sonnet-4-6"  (strip Bedrock prefix/suffix)
"claude-sonnet-4.6" → "claude-sonnet-4-6"  (dots to hyphens)
```

## Provider 选择

1. 按 priority 排序（升序，越小越优先）
2. 遍历每个 provider：
   - 检查 `credential_keys`（环境变量是否存在）
   - 如果有凭证 → 选中
   - 如果凭证为空列表 → 视为 credentialless（Ollama 等）
3. 同一 priority 下有凭据的 provider 优先于 credentialless
4. 全部无凭证 → 返回最高 priority 的 provider（api_key: None）

## 宽容回退

不在 catalog 里的模型名，如果格式是 `provider/model`：
- 查 provider_defaults → 用默认 base_url 和协议
- 否则返回 ModelNotFound

## base_url 解析

```
entry.base_url → 如果有且非空，使用
  ↓ 没有或为空
provider_defaults.base_url → 使用
  ↓ 也没有
空字符串
```
