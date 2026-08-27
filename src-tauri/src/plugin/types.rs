use std::collections::{BTreeMap, HashSet};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::provider::{
    validate_descriptor_schema, AdapterDescriptor, AdapterReference, FormField, ProviderDraft,
    ProviderRecord, BUILTIN_PLUGIN_ID, CONTRACT_MAJOR,
};

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const REGISTRY_SCHEMA_VERSION: u32 = 1;
pub const INSTALLED_SCHEMA_VERSION: u32 = 1;
pub const REGISTRY_STORE_VERSION: u32 = 1;
pub const COMPONENT_PATH: &str = "plugin.wasm";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublisherIdentity {
    pub id: String,
    pub key_id: String,
    pub algorithm: SignatureAlgorithm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignatureAlgorithm {
    Ed25519,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PluginCapability {
    ReadClaudeSettings,
    WriteClaudeSettings,
    ReadCodexConfig,
    WriteCodexConfig,
    ReadCodexAuth,
}

impl PluginCapability {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadClaudeSettings => "Read Claude Code user settings",
            Self::WriteClaudeSettings => "Change Claude Code provider routing",
            Self::ReadCodexConfig => "Read Codex user configuration",
            Self::WriteCodexConfig => "Change Codex provider routing",
            Self::ReadCodexAuth => "Read Codex API-key authentication",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestAdapter {
    pub app_id: String,
    pub adapter_id: String,
    pub display_name: String,
    pub schema_version: u32,
    pub fields: Vec<FormField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub host_api_major: u32,
    pub name: String,
    pub description: String,
    pub publisher: PublisherIdentity,
    pub component: String,
    pub adapters: Vec<ManifestAdapter>,
    pub capabilities: Vec<PluginCapability>,
    pub files: BTreeMap<String, String>,
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err("unsupported plugin manifest schema".to_owned());
        }
        validate_dotted_id(&self.id, "plugin ID")?;
        if self.id == BUILTIN_PLUGIN_ID {
            return Err("third-party plugins cannot use the built-in plugin ID".to_owned());
        }
        Version::parse(&self.version).map_err(|_| "plugin version is not semantic".to_owned())?;
        if self.host_api_major != CONTRACT_MAJOR {
            return Err("plugin requires an unsupported host API major".to_owned());
        }
        if self.name.trim().is_empty()
            || self.name.chars().count() > 80
            || self.description.chars().count() > 500
        {
            return Err("plugin presentation metadata exceeds host limits".to_owned());
        }
        validate_dotted_id(&self.publisher.id, "publisher ID")?;
        validate_simple_id(&self.publisher.key_id, "publisher key ID")?;
        if self.component != COMPONENT_PATH {
            return Err("contract major 1 requires plugin.wasm as the component".to_owned());
        }
        if self.adapters.is_empty() || self.adapters.len() > 16 {
            return Err("a plugin must contribute between 1 and 16 adapters".to_owned());
        }
        if self.capabilities.len() > 5
            || self
                .capabilities
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len()
                != self.capabilities.len()
        {
            return Err("plugin capabilities are duplicated or exceed host limits".to_owned());
        }

        let mut adapter_ids = HashSet::new();
        for adapter in &self.adapters {
            if !matches!(adapter.app_id.as_str(), "claude" | "codex") {
                return Err("contract major 1 supports only Claude Code and Codex".to_owned());
            }
            validate_dotted_id(&adapter.adapter_id, "adapter ID")?;
            if !adapter_ids.insert(&adapter.adapter_id) || adapter.schema_version == 0 {
                return Err("adapter IDs must be unique and schema versions nonzero".to_owned());
            }
            if adapter.display_name.trim().is_empty() || adapter.display_name.chars().count() > 80 {
                return Err("adapter display name exceeds host limits".to_owned());
            }
            validate_descriptor_schema(&self.descriptor(adapter))?;
            let capabilities = self.capabilities.iter().copied().collect::<HashSet<_>>();
            let required = match adapter.app_id.as_str() {
                "claude" => [
                    PluginCapability::ReadClaudeSettings,
                    PluginCapability::WriteClaudeSettings,
                ],
                "codex" => [
                    PluginCapability::ReadCodexConfig,
                    PluginCapability::WriteCodexConfig,
                ],
                _ => unreachable!(),
            };
            if !required
                .iter()
                .all(|capability| capabilities.contains(capability))
            {
                return Err("adapter is missing its required configuration capabilities".to_owned());
            }
        }

        if self.files.is_empty()
            || self.files.len() > 128
            || !self.files.contains_key(COMPONENT_PATH)
        {
            return Err("plugin payload declarations are incomplete".to_owned());
        }
        for (path, digest) in &self.files {
            validate_payload_path(path)?;
            if matches!(path.as_str(), "manifest.json" | "manifest.sig") {
                return Err("plugin payload cannot replace package metadata".to_owned());
            }
            validate_sha256(digest, "payload digest")?;
        }
        Ok(())
    }

    pub fn descriptor(&self, adapter: &ManifestAdapter) -> AdapterDescriptor {
        AdapterDescriptor {
            app_id: adapter.app_id.clone(),
            display_name: adapter.display_name.clone(),
            reference: AdapterReference {
                plugin_id: self.id.clone(),
                plugin_version: self.version.clone(),
                adapter_id: adapter.adapter_id.clone(),
                contract_major: self.host_api_major,
                schema_version: adapter.schema_version,
                extensions: serde_json::Map::new(),
            },
            fields: adapter.fields.clone(),
        }
    }

    pub fn descriptors(&self) -> Vec<AdapterDescriptor> {
        self.adapters
            .iter()
            .map(|adapter| self.descriptor(adapter))
            .collect()
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|_| "plugin manifest serialization failed".to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryPackage {
    pub manifest: PluginManifest,
    pub manifest_sha256: String,
    pub signature: String,
    pub package_url: String,
    pub package_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrustedPublisherKey {
    pub publisher_id: String,
    pub key_id: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrySource {
    pub id: String,
    pub revision: u64,
    pub label: String,
    pub index_url: String,
    pub enabled: bool,
    pub trusted_publishers: Vec<TrustedPublisherKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryDraft {
    pub id: Option<String>,
    pub expected_revision: Option<u64>,
    pub label: String,
    pub index_url: String,
    pub enabled: bool,
    pub trusted_publishers: Vec<TrustedPublisherKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplacePlugin {
    pub registry_id: String,
    pub registry_revision: u64,
    pub registry_label: String,
    pub manifest: PluginManifest,
    pub manifest_sha256: String,
    pub package_sha256: String,
    pub publisher_key_sha256: String,
    pub installed: Option<InstalledPlugin>,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryRefreshFailure {
    pub registry_id: String,
    pub registry_label: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceCatalog {
    pub plugins: Vec<MarketplacePlugin>,
    pub failures: Vec<RegistryRefreshFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallSelection {
    pub registry_id: String,
    pub registry_revision: u64,
    pub plugin_id: String,
    pub version: String,
    pub manifest_sha256: String,
    pub package_sha256: String,
    pub publisher_key_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledPlugin {
    pub id: String,
    pub version: String,
    pub previous: Option<RetainedPluginVersion>,
    pub registry_id: String,
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub publisher: PublisherIdentity,
    pub publisher_key_sha256: String,
    pub granted_capabilities: Vec<PluginCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedPluginVersion {
    pub version: String,
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub publisher: PublisherIdentity,
    pub publisher_key_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledFile {
    pub version: u32,
    pub plugins: Vec<InstalledPlugin>,
}

impl Default for InstalledFile {
    fn default() -> Self {
        Self {
            version: INSTALLED_SCHEMA_VERSION,
            plugins: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryFile {
    pub version: u32,
    pub registries: Vec<RegistrySource>,
}

impl Default for RegistryFile {
    fn default() -> Self {
        Self {
            version: REGISTRY_STORE_VERSION,
            registries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PluginSlot {
    ClaudeSettings,
    CodexConfig,
    CodexAuth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSnapshot {
    pub slot: PluginSlot,
    pub contents: Option<String>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginRoute {
    pub api_key: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "payload",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PluginRequest {
    Validate {
        contract_major: u32,
        app_id: String,
        adapter_id: String,
        settings: serde_json::Map<String, serde_json::Value>,
    },
    Import {
        contract_major: u32,
        app_id: String,
        adapter_id: String,
        snapshots: Vec<PluginSnapshot>,
    },
    Plan {
        contract_major: u32,
        provider: ProviderRecord,
        snapshots: Vec<PluginSnapshot>,
    },
    Current {
        contract_major: u32,
        app_id: String,
        adapter_id: String,
        settings: serde_json::Map<String, serde_json::Value>,
        snapshots: Vec<PluginSnapshot>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "payload",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PluginResponse {
    Valid,
    Imported { provider: ProviderDraft },
    Routed { route: PluginRoute },
    Current { matches: bool },
}

pub fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{label} is not a lowercase SHA-256 digest"));
    }
    Ok(())
}

pub fn validate_payload_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 240
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("plugin payload path is unsafe".to_owned());
    }
    Ok(())
}

pub fn validate_dotted_id(value: &str, label: &str) -> Result<(), String> {
    if value.len() < 3
        || value.len() > 128
        || !value.contains('.')
        || value.split('.').any(|part| {
            part.is_empty()
                || part.starts_with('-')
                || part.ends_with('-')
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return Err(format!("{label} is not a safe dotted identifier"));
    }
    Ok(())
}

fn validate_simple_id(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("{label} is not a safe identifier"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_response_accepts_only_the_typed_provider_route() {
        let response: PluginResponse = serde_json::from_str(
            r#"{"operation":"routed","payload":{"route":{"apiKey":"secret","baseUrl":null,"model":null}}}"#,
        )
        .unwrap();
        assert!(matches!(response, PluginResponse::Routed { .. }));

        assert!(serde_json::from_str::<PluginResponse>(
            r#"{"operation":"routed","payload":{"route":{"apiKey":"secret","baseUrl":null,"model":null,"command":"sh"}}}"#,
        )
        .is_err());
        assert!(serde_json::from_str::<PluginResponse>(
            r#"{"operation":"planned","payload":{"plan":{"contractMajor":1,"appId":"claude","writes":[]}}}"#,
        )
        .is_err());
    }
}
