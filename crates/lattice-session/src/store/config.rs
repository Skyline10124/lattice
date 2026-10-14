// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn 接受时冻结的非秘密执行配置、工具目录及协议上限。
//! 摘要保持既有域分离与规范 JSON 编码，配置不保存凭据。

use sha2::{Digest, Sha256};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Result, invalid};

/// Code-defined model-step safety limit; not profile-configurable
/// (ADR 0005 §10).
pub const MAX_MODEL_STEPS: u32 = 32;

/// Code-defined per-step model-attempt limit: one initial attempt plus the
/// profile contract's at-most ten retries.
pub const MAX_MODEL_ATTEMPTS: u32 = 11;

/// Canonical catalog snapshot byte limit (1 MiB, ADR 0005 §8). A snapshot of
/// exactly this length is legal; one byte more fails closed.
pub const MAX_CATALOG_SNAPSHOT_BYTES: usize = 1024 * 1024;

/// User input byte bound, aligned with the assistant content bound of one
/// attempt (`lattice-conversation` limits, ADR 0005 §10). Counted as UTF-8
/// bytes of the user message's text parts.
pub const MAX_USER_INPUT_BYTES: usize = 2 * 1024 * 1024;

const CATALOG_DOMAIN: &[u8] = b"lattice-tool-catalog-snapshot-v1\0";
const CONFIG_DOMAIN: &[u8] = b"lattice-frozen-config-digest-v1\0";

fn digest_with_domain(domain: &[u8], canonical_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((canonical_bytes.len() as u64).to_be_bytes());
    hasher.update(canonical_bytes);
    hasher.finalize().into()
}

/// Frozen execution limits (ADR 0005 §8/§10). The Turn deadline itself is a
/// live monotonic construction over `timeout_secs` and is never persisted;
/// reopen does not reconstruct remaining time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnExecutionLimits {
    /// Frozen provider/Tool deadline in seconds.
    pub timeout_secs: u64,
    /// Recorded model-step limit; must equal [`MAX_MODEL_STEPS`] (it is
    /// code-defined, not profile-configurable).
    pub max_model_steps: u32,
    /// Frozen per-step provider attempt (retry) budget.
    pub max_model_attempts: u32,
}

impl TurnExecutionLimits {
    /// Validate the frozen limits: the model-step limit is code-defined and
    /// the attempt budget is between one and [`MAX_MODEL_ATTEMPTS`].
    pub fn validate(&self) -> Result<()> {
        if self.max_model_steps != MAX_MODEL_STEPS {
            let actual = self.max_model_steps;
            return Err(invalid(
                "limits",
                &format!(
                    "max_model_steps must be the code-defined {MAX_MODEL_STEPS}, got {actual}"
                ),
            ));
        }
        if !(1..=MAX_MODEL_ATTEMPTS).contains(&self.max_model_attempts) {
            let actual = self.max_model_attempts;
            return Err(invalid(
                "limits",
                &format!(
                    "max_model_attempts must be between 1 and {MAX_MODEL_ATTEMPTS}, got {actual}"
                ),
            ));
        }
        Ok(())
    }
}

/// One enabled Tool in the frozen catalog snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCatalogEntry {
    /// Tool method name.
    pub name: String,
    /// Descriptor identity (digest hex) of the embedded component.
    pub descriptor_identity: String,
    /// Provider-facing description.
    pub description: String,
    /// Canonical JSON input schema.
    pub input_schema: Value,
}

/// Content-addressed provider-neutral snapshot of the enabled Tool catalog
/// (ADR 0005 §8): ordered names, descriptor identities, descriptions,
/// canonical input schemas, and the runtime build identity, plus its digest.
/// Provider wire projection is not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCatalogSnapshot {
    /// Runtime build identity of the closed Tool catalog.
    pub runtime_build_id: String,
    /// Ordered entries.
    pub entries: Vec<ToolCatalogEntry>,
    /// SHA-256 over the domain separator, explicit byte length, and the
    /// canonical JSON bytes of the entry list.
    #[serde(with = "super::hex_digest")]
    pub digest: [u8; 32],
}

impl ToolCatalogSnapshot {
    /// Build and validate a snapshot: names must be non-empty and unique,
    /// and the canonical bytes must fit the 1 MiB protocol limit (exact
    /// boundary legal, plus one rejected).
    pub fn new(runtime_build_id: String, mut entries: Vec<ToolCatalogEntry>) -> Result<Self> {
        for entry in &entries {
            if entry.name.is_empty() {
                return Err(invalid("catalog", "catalog entry name must not be empty"));
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        if entries.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(invalid("catalog", "catalog entry names must be unique"));
        }
        let (canonical, digest) = Self::canonicalize(&runtime_build_id, &entries)?;
        if canonical.len() > MAX_CATALOG_SNAPSHOT_BYTES {
            return Err(invalid(
                "catalog",
                &format!(
                    "catalog snapshot canonical bytes exceed the 1 MiB limit: {} > {}",
                    canonical.len(),
                    MAX_CATALOG_SNAPSHOT_BYTES
                ),
            ));
        }
        Ok(Self {
            runtime_build_id,
            entries,
            digest,
        })
    }

    /// Validate a deserialized snapshot: the recorded digest must describe
    /// the recorded entries and the byte limit must hold.
    pub fn validate(&self) -> Result<()> {
        let (canonical, digest) = Self::canonicalize(&self.runtime_build_id, &self.entries)?;
        if canonical.len() > MAX_CATALOG_SNAPSHOT_BYTES {
            return Err(invalid(
                "catalog",
                "catalog snapshot exceeds the 1 MiB limit",
            ));
        }
        if digest != self.digest {
            return Err(invalid(
                "catalog",
                "catalog snapshot digest does not describe its entries",
            ));
        }
        Ok(())
    }

    fn canonicalize(
        runtime_build_id: &str,
        entries: &[ToolCatalogEntry],
    ) -> Result<(String, [u8; 32])> {
        let value = serde_json::to_value((runtime_build_id, entries))
            .map_err(|_| invalid("catalog", "cannot serialize catalog"))?;
        let canonical = lancet_protocol::canonical_json(&value);
        let digest = digest_with_domain(CATALOG_DOMAIN, canonical.as_bytes());
        Ok((canonical, digest))
    }
}

/// 配置表按内容摘要去重保存的大体积配置：系统提示与启用工具目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredTurnConfig {
    pub system_prompt: Option<String>,
    pub catalog: ToolCatalogSnapshot,
}

impl StoredTurnConfig {
    pub fn digest(&self) -> Result<[u8; 32]> {
        let value = serde_json::to_value(self)
            .map_err(|_| invalid("config", "cannot serialize turn config"))?;
        let canonical = lancet_protocol::canonical_json(&value);
        Ok(digest_with_domain(CONFIG_DOMAIN, canonical.as_bytes()))
    }

    pub fn validate(&self) -> Result<()> {
        if self
            .system_prompt
            .as_ref()
            .is_some_and(|s| s.len() > 2 * 1024 * 1024)
        {
            return Err(invalid("config", "system prompt exceeds 2 MiB bound"));
        }
        self.catalog.validate()
    }
}

/// The complete frozen effective configuration one accepted Turn runs under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenTurnConfig {
    /// Turn 的 Agent 名，用于日志及当前位置的 Agent 投影。
    pub agent_name: String,
    /// Effective model id.
    pub model: String,
    /// Effective provider id.
    pub provider: String,
    /// Effective system prompt, when one is configured.
    pub system_prompt: Option<String>,
    /// Frozen enabled Tool catalog.
    pub catalog: ToolCatalogSnapshot,
    /// Frozen execution limits.
    pub limits: TurnExecutionLimits,
}

impl FrozenTurnConfig {
    /// 提取存入配置表的部分。
    pub fn stored(&self) -> StoredTurnConfig {
        StoredTurnConfig {
            system_prompt: self.system_prompt.clone(),
            catalog: self.catalog.clone(),
        }
    }

    /// 配置表的内容摘要，由系统提示与工具目录算出。
    pub fn digest(&self) -> Result<[u8; 32]> {
        self.stored().digest()
    }

    /// Validate a deserialized configuration: limits match the code-defined
    /// bound, the catalog snapshot is self-consistent, and every string field
    /// is non-empty where required.
    pub fn validate(&self) -> Result<()> {
        if self.agent_name.is_empty() || self.model.is_empty() || self.provider.is_empty() {
            return Err(invalid(
                "config",
                "frozen config agent/model/provider must not be empty",
            ));
        }
        self.limits.validate()?;
        self.stored().validate()?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::super::StoreError;
    use super::*;

    fn catalog() -> ToolCatalogSnapshot {
        ToolCatalogSnapshot::new(
            "build-1".to_string(),
            vec![ToolCatalogEntry {
                name: "observe.workspace".to_string(),
                descriptor_identity: "a".repeat(64),
                description: "observe".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        )
        .expect("catalog")
    }

    fn limits() -> TurnExecutionLimits {
        TurnExecutionLimits {
            timeout_secs: 120,
            max_model_steps: MAX_MODEL_STEPS,
            max_model_attempts: 3,
        }
    }

    fn config() -> FrozenTurnConfig {
        FrozenTurnConfig {
            agent_name: "coder".to_string(),
            model: "m".to_string(),
            provider: "p".to_string(),
            system_prompt: None,
            catalog: catalog(),
            limits: limits(),
        }
    }

    #[test]
    fn catalog_digest_is_self_describing() {
        let snapshot = catalog();
        snapshot.validate().expect("valid");
        let mut tampered = snapshot.clone();
        tampered.entries[0].description = "changed".to_string();
        assert!(matches!(
            tampered.validate(),
            Err(StoreError::InvalidInput { .. })
        ));
    }

    #[test]
    fn stored_config_digest_depends_on_prompt_and_catalog() {
        let base = config();
        base.validate().expect("clean config passes");
        let mut same_stored = base.clone();
        same_stored.agent_name = "other".to_string();
        assert_eq!(base.digest().unwrap(), same_stored.digest().unwrap());
        let mut different_prompt = base.clone();
        different_prompt.system_prompt = Some("prompt".to_string());
        assert_ne!(base.digest().unwrap(), different_prompt.digest().unwrap());
        let mut bad = config();
        bad.system_prompt = Some("x".repeat(2 * 1024 * 1024 + 1));
        assert!(matches!(
            bad.validate(),
            Err(StoreError::InvalidInput { .. })
        ));
    }

    #[test]
    fn limits_must_carry_the_code_defined_step_limit() {
        let mut limits = limits();
        limits.max_model_steps = MAX_MODEL_STEPS + 1;
        assert!(limits.validate().is_err());
        limits.max_model_steps = MAX_MODEL_STEPS;
        limits.max_model_attempts = 0;
        assert!(limits.validate().is_err());
        limits.max_model_attempts = MAX_MODEL_ATTEMPTS + 1;
        assert!(limits.validate().is_err());
        limits.max_model_attempts = MAX_MODEL_ATTEMPTS;
        limits.validate().expect("last valid attempt budget");
    }
}
