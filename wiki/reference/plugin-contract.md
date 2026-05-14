# Plugin Runtime Contract

Runtime defines the plugin contract. Official plugin implementations live in `LATTICE-Plugins`.

## Typed Plugin

```rust
pub trait Plugin: Send + Sync {
    type Input: Serialize + DeserializeOwned + Send;
    type Output: Serialize + DeserializeOwned + Send;

    fn name(&self) -> &str;
    fn system_prompt(&self) -> &str;
    fn to_prompt(&self, input: &Self::Input) -> String;
    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError>;
}
```

`parse_output()` has a default JSON implementation. Plugins only override it when they need custom parsing.

## Erased Plugin

`ErasedPlugin` lets `PluginRegistry` store heterogeneous typed plugins. This is the interface used by plugin DAG execution.

## Behavior

Runtime behavior policies decide how to handle low confidence, parse errors and retries:

- `StrictBehavior`
- `YoloBehavior`

Behavior mode types live in `lattice-core` so profiles, bundles and callers do not depend on upper crates.

## Manifest Plugins

Runtime can load declarative manifest plugins from local directories. This is useful for local prompt/template experiments and does not require adding official plugin code to Runtime.

Official compiled plugins are registered by `LATTICE-Plugins`.
