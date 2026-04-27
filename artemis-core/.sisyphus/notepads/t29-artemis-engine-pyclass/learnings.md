# T29: ArtemisEngine PyClass Learnings

## Changes Made

### 1. `src/engine.rs` — Full Python-facing engine API
- Added `PyResolvedModel` pyclass (Python-facing version of `ResolvedModel` with string `api_protocol` field, masked `api_key` in repr)
- Added `register_model()` pyethod — accepts canonical_id, display_name, provider_id, api_model_id, base_url, api_protocol_str; builds `CatalogProviderEntry` + `ModelCatalogEntry`; registers with both router (`register_catalog_entry`) and registry (`register` + `MockProvider`)
- Added `resolve_model()` pyethod — delegates to `ModelRegistry::resolve()`, returns `PyResolvedModel`

### 2. `src/provider.rs` — ModelRegistry enhancements
- `list_models()` now combines `router.list_models()` (catalog + custom_models) with locally registered models
- Added `register_catalog_entry()` — forwards to `router.register_model()` for name resolution
- Added `resolve()` — forwards to `router.resolve()` for model resolution

### 3. `src/router.rs` — Resolution behavior fix
- Changed fallback when no credentials found: instead of calling `resolve_permissive()` (which requires `provider/model` format), now returns the first provider entry with `api_key: None`
- This allows `resolve_model()` to return metadata (provider, URL, protocol) even without valid credentials

### 4. `src/lib.rs`
- Registered `PyResolvedModel` as a Python-accessible class

### 5. Test updates
- `test_exhaustion_no_credentials` — updated to expect success with `api_key: None`
- `test_register_multiple_and_list` — updated to use `>=` instead of exact count (catalog models now included)

## Key Design Decisions
- `register_model()` creates a `MockProvider` because real HTTP providers are not yet available (Wave 5+)
- `resolve_model()` returns metadata even without credentials — useful for "what would be used" queries
- `ApiProtocol` enum is stringified to `"OpenAiChat"`, `"AnthropicMessages"`, etc. in `PyResolvedModel`
