# Catalog 与 Provider

## 什么是 Catalog

`artemis-core/src/catalog/data.json` — 编译时嵌入的模型黄页。98 个模型、37 别名、27 个 provider 默认配置（其中 23 个带 base_url）。

## 模型条目

```json
{
  "canonical_id": "claude-sonnet-4-6",
  "display_name": "Claude Sonnet 4.6",
  "context_length": 200000,
  "providers": [
    {
      "provider_id": "anthropic",
      "api_model_id": "claude-sonnet-4-6",
      "priority": 1,
      "credential_keys": {"api_key": "ANTHROPIC_API_KEY"},
      "base_url": null,
      "api_protocol": "anthropic_messages"
    }
  ],
  "aliases": ["sonnet"]
}
```

## Provider 默认值

```json
{
  "deepseek": {
    "api_protocol": "chat_completions",
    "credential_keys": {"api_key": "DEEPSEEK_API_KEY"},
    "base_url": "https://api.deepseek.com"
  }
}
```

模型的 provider entry 里 `base_url: null` 时，回退到这里。

## 别名

```
"sonnet" → "claude-sonnet-4-6"
"gpt5" → "gpt-5.4"
"deepseek" → "deepseek-v4-pro"
"haiku" → "claude-haiku-4-5"
```

37 个别名，用户说任意一个都能解析。

## Catalog 维护

data.json 手动维护。修改后 `cargo build` 自动嵌入。无需运行脚本。
