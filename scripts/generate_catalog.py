#!/usr/bin/env python3
"""Generate data.json from hermes model-centric branch."""
import subprocess, json, sys, os, re, textwrap
from pathlib import Path

HERMES_DIR = Path.home() / ".hermes" / "hermes-agent"
ARTEMIS_DIR = Path(__file__).resolve().parent.parent / "artemis-core"
OUTPUT = ARTEMIS_DIR / "src" / "catalog" / "data.json"

API_MODE_MAP = {
    "chat_completions": "chat_completions",
    "anthropic_messages": "anthropic_messages",
    "anthropic": "anthropic_messages",
    "codex_responses": "codex_responses",
    "codex": "codex_responses",
    "bedrock_converse": "bedrock_converse",
    "bedrock": "bedrock_converse",
    "gemini": "gemini_generate_content",
    "acp": "chat_completions",  # ACP uses chat_completions bridge
}

# Provider IDs to filter out (no Rust transport implementation yet)
FILTER_PROVIDERS = {"bedrock", "openai-codex"}

def extract_dict(source, var_name):
    """Extract dict literal assignment from Python source."""
    # Try annotated version first
    pattern = rf'{var_name}\s*:\s*dict\[[^\]]+\]\s*=\s*\{{'
    match = re.search(pattern, source)
    if not match:
        # Try without type annotation
        pattern = rf'{var_name}\s*=\s*\{{'
        match = re.search(pattern, source)
    if not match:
        raise ValueError(f"Could not find {var_name}")
    
    start = match.start() + match.group().rfind('{')
    depth = 0
    i = start
    while i < len(source):
        if source[i] == '{': depth += 1
        elif source[i] == '}':
            depth -= 1
            if depth == 0:
                return source[start:i+1]
        i += 1
    raise ValueError(f"Unterminated dict for {var_name}")

def main():
    models_py = subprocess.check_output(
        ["git", "show", "model-centric:hermes_cli/models.py"],
        cwd=HERMES_DIR, text=True
    )
    
    # Pre-define constants from models.py that dict bodies reference
    COPILOT_AGENT_FLAG = "copilot"
    COPILOT_BASE_URL = "https://api.githubcopilot.com"
    COPILOT_ENTERPRISE_BASE_URL = "https://api.enterprise.githubcopilot.com"
    COPILOT_AUTHORITY = "https://api.github.com/copilot/token"
    COPILOT_ENTERPRISE_AUTHORITY = "https://api.github.com/copilot/token"
    COPILOT_PROVIDER_ID = "copilot"
    
    # Mock dataclasses for execution
    exec_code = textwrap.dedent("""
    from dataclasses import dataclass, field
    from typing import List, Dict, Optional

    @dataclass
    class CatalogProviderEntry:
        provider_id: str
        api_model_id: str
        priority: int = 1
        weight: int = 1
        credential_keys: dict = field(default_factory=dict)
        base_url: Optional[str] = None
        api_mode: str = "chat_completions"
        provider_specific: dict = field(default_factory=dict)

    @dataclass
    class ModelCatalogEntry:
        canonical_id: str
        display_name: str
        description: str = ""
        context_length: int = 0
        capabilities: list = field(default_factory=list)
        providers: list = field(default_factory=list)
        aliases: list = field(default_factory=list)
    """)
    exec(exec_code, globals())
    
    # Use globals() (which now includes mock dataclasses) plus constants
    exec_ns = dict(globals())
    exec_ns.update({
        'COPILOT_BASE_URL': COPILOT_BASE_URL,
        'COPILOT_ENTERPRISE_BASE_URL': COPILOT_ENTERPRISE_BASE_URL,
        'COPILOT_AUTHORITY': COPILOT_AUTHORITY,
        'COPILOT_ENTERPRISE_AUTHORITY': COPILOT_ENTERPRISE_AUTHORITY,
        'COPILOT_PROVIDER_ID': COPILOT_PROVIDER_ID,
        'COPILOT_AGENT_FLAG': COPILOT_AGENT_FLAG,
    })
    
    alias_body = extract_dict(models_py, '_MODEL_ALIASES')
    aliases_ns = dict(exec_ns)
    exec(f'_MODEL_ALIASES = {alias_body}', aliases_ns)
    aliases = aliases_ns['_MODEL_ALIASES']
    
    defaults_body = extract_dict(models_py, '_PROVIDER_CATALOG_DEFAULTS')
    defaults_ns = dict(exec_ns)
    exec(f'_PROVIDER_CATALOG_DEFAULTS = {defaults_body}', defaults_ns)
    defaults_raw = defaults_ns['_PROVIDER_CATALOG_DEFAULTS']
    
    catalog_body = extract_dict(models_py, '_MODEL_CATALOG')
    catalog_ns = dict(exec_ns)
    exec(f'_MODEL_CATALOG = {catalog_body}', catalog_ns)
    catalog_raw = catalog_ns['_MODEL_CATALOG']
    
    # Build output
    data = {"models": [], "aliases": dict(aliases), "provider_defaults": {}}
    
    for canonical_id, entry in catalog_raw.items():
        providers = []
        for p in entry.providers:
            if p.provider_id in FILTER_PROVIDERS:
                continue
            providers.append({
                "provider_id": p.provider_id,
                "api_model_id": p.api_model_id,
                "priority": getattr(p, 'priority', 1),
                "weight": getattr(p, 'weight', 1),
                "credential_keys": dict(getattr(p, 'credential_keys', {})),
                "base_url": getattr(p, 'base_url', None),
                "api_protocol": API_MODE_MAP.get(getattr(p, 'api_mode', 'chat_completions'), 'chat_completions'),
                "provider_specific": dict(getattr(p, 'provider_specific', {})),
            })
        if providers:  # Only include if at least one non-filtered provider remains
            data["models"].append({
                "canonical_id": canonical_id,
                "display_name": entry.display_name,
                "description": getattr(entry, 'description', ''),
                "context_length": getattr(entry, 'context_length', 0),
                "capabilities": list(getattr(entry, 'capabilities', [])),
                "providers": providers,
                "aliases": list(getattr(entry, 'aliases', [])),
            })
    
    for pid, pdefaults in defaults_raw.items():
        data["provider_defaults"][pid] = {
            "api_protocol": API_MODE_MAP.get(pdefaults.get('api_mode', 'chat_completions'), 'chat_completions'),
            "credential_keys": dict(pdefaults.get('credential_keys', {})),
            "base_url": pdefaults.get('base_url', ''),
        }
    
    print(f"Generated {len(data['models'])} models, {len(data['aliases'])} aliases, {len(data['provider_defaults'])} provider defaults")
    
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    with open(OUTPUT, 'w') as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
    print(f"Wrote {OUTPUT} ({os.path.getsize(OUTPUT)} bytes)")

if __name__ == '__main__':
    main()
