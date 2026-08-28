use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};

use futures_util::future::join_all;
use reqwest::{redirect::Policy, Client};
use semver::Version;
use url::{Host, Url};

use crate::provider::{
    adapter_for_reference as built_in_adapter, built_in_adapters, validate_settings,
    AdapterDescriptor, AdapterReference, ProviderRecord, CONTRACT_MAJOR,
};

use super::{
    package::{
        prepare_package, publisher_key_sha256, validate_trusted_key, verify_registry_entry,
        PreparedPackage, MAX_PACKAGE_BYTES,
    },
    runtime::PluginRuntime,
    state::{Activation, PluginState},
    InstallSelection, InstalledPlugin, MarketplaceCatalog, MarketplacePlugin, PluginError,
    PluginManifest, PluginRequest, PluginResponse, RegistryDraft, RegistryPackage,
    RegistryRefreshFailure, RegistrySource, REGISTRY_SCHEMA_VERSION,
};

const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
const MAX_INDEX_PACKAGES: usize = 2_000;

pub struct PreparedInstall {
    registry: RegistrySource,
    package: RegistryPackage,
    prepared: PreparedPackage,
    publisher_key_sha256: String,
}

struct FetchedIndex {
    packages: Vec<RegistryPackage>,
    invalid_entries: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawRegistryIndex {
    schema_version: u32,
    packages: Vec<Box<serde_json::value::RawValue>>,
}

pub struct PluginManager {
    state: PluginState,
    runtime: PluginRuntime,
    client: Client,
}

impl PluginManager {
    pub fn new(root: PathBuf) -> Result<Self, PluginError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() > 3 {
                    return attempt.error("too many redirects");
                }
                if !redirect_allowed(attempt.url(), attempt.previous()) {
                    return attempt.error("redirect violates the plugin network policy");
                }
                attempt.follow()
            }))
            .user_agent("cc-switch-lite/0.1 plugin-marketplace")
            .build()
            .map_err(PluginError::Network)?;
        let state = PluginState::new(root);
        state.recover()?;
        Ok(Self {
            state,
            runtime: PluginRuntime::new()?,
            client,
        })
    }

    pub fn registries(&self) -> Result<Vec<RegistrySource>, PluginError> {
        self.state.list_registries()
    }

    pub fn save_registry(&self, draft: RegistryDraft) -> Result<RegistrySource, PluginError> {
        let normalized_url = registry_url(&draft.index_url)?.to_string();
        let mut trusted = HashSet::new();
        for key in &draft.trusted_publishers {
            validate_trusted_key(key)?;
            if !trusted.insert((&key.publisher_id, &key.key_id)) {
                return Err(PluginError::Invalid(
                    "trusted publisher keys must be unique".to_owned(),
                ));
            }
        }
        self.state.save_registry(draft, normalized_url)
    }

    pub fn remove_registry(&self, id: &str, expected_revision: u64) -> Result<(), PluginError> {
        self.state.remove_registry(id, expected_revision)
    }

    pub fn installed(&self) -> Result<Vec<InstalledPlugin>, PluginError> {
        self.state.list_installed()
    }

    pub fn adapters(&self) -> Vec<AdapterDescriptor> {
        let mut descriptors = built_in_adapters();
        if let Ok(installed_plugins) = self.state.list_installed() {
            for installed in installed_plugins {
                if let Ok(manifest) = self.checked_manifest(&installed, &installed.version) {
                    descriptors.extend(manifest.descriptors());
                }
            }
        }
        descriptors
    }

    pub fn adapter_for_reference(
        &self,
        app_id: &str,
        reference: &AdapterReference,
    ) -> Result<Option<AdapterDescriptor>, PluginError> {
        if let Some(adapter) = built_in_adapter(app_id, reference) {
            return Ok(Some(adapter));
        }
        let installed = match self.state.installed(&reference.plugin_id) {
            Ok(installed) => installed,
            Err(PluginError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        if reference.plugin_version != installed.version
            && installed
                .previous
                .as_ref()
                .map(|version| version.version.as_str())
                != Some(&reference.plugin_version)
        {
            return Ok(None);
        }
        let manifest = self.checked_manifest(&installed, &reference.plugin_version)?;
        Ok(manifest
            .descriptors()
            .into_iter()
            .find(|adapter| adapter.app_id == app_id && adapter.reference.same_identity(reference)))
    }

    pub fn capabilities_for_reference(
        &self,
        app_id: &str,
        reference: &AdapterReference,
    ) -> Result<Vec<super::PluginCapability>, PluginError> {
        let Some(_) = self.adapter_for_reference(app_id, reference)? else {
            return Err(PluginError::NotFound);
        };
        if reference.plugin_id == crate::provider::BUILTIN_PLUGIN_ID {
            return Ok(Vec::new());
        }
        let installed = self.state.installed(&reference.plugin_id)?;
        let manifest = self.checked_manifest(&installed, &reference.plugin_version)?;
        if installed.granted_capabilities != manifest.capabilities {
            return Err(PluginError::InvalidState(
                "installed capabilities differ from the signed manifest".to_owned(),
            ));
        }
        Ok(installed.granted_capabilities)
    }

    pub fn validate_provider(
        &self,
        descriptor: &AdapterDescriptor,
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), PluginError> {
        validate_settings(descriptor, settings).map_err(PluginError::Invalid)?;
        if descriptor.reference.plugin_id == crate::provider::BUILTIN_PLUGIN_ID {
            return Ok(());
        }
        let response = self.invoke(
            &descriptor.reference,
            &PluginRequest::Validate {
                contract_major: CONTRACT_MAJOR,
                app_id: descriptor.app_id.clone(),
                adapter_id: descriptor.reference.adapter_id.clone(),
                settings: settings.clone(),
            },
        )?;
        if response != PluginResponse::Valid {
            return Err(PluginError::Runtime);
        }
        Ok(())
    }

    pub fn invoke(
        &self,
        reference: &AdapterReference,
        request: &PluginRequest,
    ) -> Result<PluginResponse, PluginError> {
        let installed = self.state.installed(&reference.plugin_id)?;
        if reference.plugin_version != installed.version
            && installed
                .previous
                .as_ref()
                .map(|version| version.version.as_str())
                != Some(&reference.plugin_version)
        {
            return Err(PluginError::NotFound);
        }
        let manifest = self.checked_manifest(&installed, &reference.plugin_version)?;
        if reference.contract_major != manifest.host_api_major
            || !manifest.descriptors().iter().any(|adapter| {
                adapter.reference.same_identity(reference)
                    && adapter.app_id == reference_app(request)
            })
        {
            return Err(PluginError::Invalid(
                "provider adapter does not match the installed plugin".to_owned(),
            ));
        }
        let component_digest = manifest
            .files
            .get(&manifest.component)
            .ok_or_else(|| PluginError::InvalidState("component digest is missing".to_owned()))?;
        let component_path = self.state.component_path(&manifest);
        self.runtime
            .invoke(&component_path, component_digest, request)
    }

    pub async fn refresh(&self) -> Result<MarketplaceCatalog, PluginError> {
        let installed = self.state.list_installed()?;
        let mut plugins: Vec<MarketplacePlugin> = Vec::new();
        let mut failures = Vec::new();
        let registries = self
            .state
            .list_registries()?
            .into_iter()
            .filter(|registry| registry.enabled)
            .collect::<Vec<_>>();
        let indexes = join_all(registries.into_iter().map(|registry| async move {
            let index = self.fetch_index(&registry).await;
            (registry, index)
        }))
        .await;
        for (registry, index) in indexes {
            match index {
                Ok(index) => {
                    for package in index.packages {
                        let candidate = MarketplacePlugin {
                            registry_id: registry.id.clone(),
                            registry_revision: registry.revision,
                            registry_label: registry.label.clone(),
                            manifest: package.manifest.clone(),
                            manifest_sha256: package.manifest_sha256.clone(),
                            package_sha256: package.package_sha256.clone(),
                            publisher_key_sha256: publisher_key_sha256(
                                &registry,
                                &package.manifest.publisher,
                            )?,
                            installed: installed
                                .iter()
                                .find(|plugin| plugin.id == package.manifest.id)
                                .cloned(),
                            permissions: package
                                .manifest
                                .capabilities
                                .iter()
                                .map(|capability| capability.label().to_owned())
                                .collect(),
                        };
                        let existing = plugins.iter().position(|entry| {
                            entry.registry_id == candidate.registry_id
                                && entry.manifest.id == candidate.manifest.id
                        });
                        if let Some(index) = existing {
                            if catalog_candidate_is_better(&candidate, &plugins[index]) {
                                plugins[index] = candidate;
                            }
                        } else {
                            plugins.push(candidate);
                        }
                    }
                    if index.invalid_entries {
                        failures.push(RegistryRefreshFailure {
                            registry_id: registry.id.clone(),
                            registry_label: registry.label.clone(),
                            message: "Some registry entries failed verification and were skipped."
                                .to_owned(),
                        });
                    }
                }
                Err(_) => failures.push(RegistryRefreshFailure {
                    registry_id: registry.id,
                    registry_label: registry.label,
                    message: "Registry refresh failed verification or download checks.".to_owned(),
                }),
            }
        }
        plugins.sort_by(|left, right| {
            left.manifest
                .name
                .to_lowercase()
                .cmp(&right.manifest.name.to_lowercase())
                .then_with(|| left.registry_label.cmp(&right.registry_label))
        });
        Ok(MarketplaceCatalog { plugins, failures })
    }

    pub async fn prepare_install(
        &self,
        selection: &InstallSelection,
    ) -> Result<PreparedInstall, PluginError> {
        let registry = self.state.registry(&selection.registry_id)?;
        if !registry.enabled || registry.revision != selection.registry_revision {
            return Err(PluginError::Conflict);
        }
        let index_url = registry_url(&registry.index_url)?;
        let package = self
            .fetch_index(&registry)
            .await?
            .packages
            .into_iter()
            .find(|package| {
                package.manifest.id == selection.plugin_id
                    && package.manifest.version == selection.version
                    && package.manifest_sha256 == selection.manifest_sha256
                    && package.package_sha256 == selection.package_sha256
            })
            .ok_or(PluginError::Conflict)?;
        let publisher_key_sha256 = publisher_key_sha256(&registry, &package.manifest.publisher)?;
        if publisher_key_sha256 != selection.publisher_key_sha256 {
            return Err(PluginError::Conflict);
        }
        let package_url = download_url(&index_url, &package.package_url)?;
        let archive = self.fetch_bytes(package_url, MAX_PACKAGE_BYTES).await?;
        let staging = self.state.staging_path()?;
        let prepared = prepare_package(&registry, &package, &archive, staging)?;
        let component_digest = prepared
            .manifest
            .files
            .get(&prepared.manifest.component)
            .ok_or_else(|| PluginError::Verification("component digest is missing".to_owned()))?;
        self.runtime.validate_component(
            &prepared.staging_path.join(&prepared.manifest.component),
            component_digest,
        )?;
        Ok(PreparedInstall {
            registry,
            package,
            prepared,
            publisher_key_sha256,
        })
    }

    pub fn activate(
        &self,
        prepared: PreparedInstall,
        approved_capabilities: &[super::PluginCapability],
        providers: &[ProviderRecord],
    ) -> Result<InstalledPlugin, PluginError> {
        if approved_capabilities != prepared.prepared.manifest.capabilities {
            return Err(PluginError::Invalid(
                "approved permissions do not match the signed plugin manifest".to_owned(),
            ));
        }
        let plugin_in_use = providers
            .iter()
            .any(|provider| provider.adapter.plugin_id == prepared.prepared.manifest.id);
        self.state.activate(Activation {
            staging: &prepared.prepared.staging_path,
            registry: &prepared.registry,
            manifest: &prepared.prepared.manifest,
            package_sha256: &prepared.package.package_sha256,
            manifest_sha256: &prepared.package.manifest_sha256,
            signature: &prepared.package.signature,
            publisher_key_sha256: &prepared.publisher_key_sha256,
            plugin_in_use,
        })
    }

    pub fn remove(&self, plugin_id: &str, providers: &[ProviderRecord]) -> Result<(), PluginError> {
        if providers
            .iter()
            .any(|provider| provider.adapter.plugin_id == plugin_id)
        {
            return Err(PluginError::Invalid(
                "remove providers owned by this plugin before uninstalling it".to_owned(),
            ));
        }
        self.state.remove_installed(plugin_id)
    }

    async fn fetch_index(&self, registry: &RegistrySource) -> Result<FetchedIndex, PluginError> {
        let url = registry_url(&registry.index_url)?;
        let bytes = self.fetch_bytes(url.clone(), MAX_INDEX_BYTES).await?;
        let index: RawRegistryIndex = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::Verification("registry index JSON is invalid".to_owned()))?;
        if index.schema_version != REGISTRY_SCHEMA_VERSION
            || index.packages.len() > MAX_INDEX_PACKAGES
        {
            return Err(PluginError::Verification(
                "registry index schema or package count is unsupported".to_owned(),
            ));
        }
        let (packages, mut invalid_entries) = decode_registry_packages(index.packages);
        let mut identities: HashMap<(String, String), Option<RegistryPackage>> = HashMap::new();
        for package in packages {
            if verify_registry_entry(registry, &package).is_err()
                || download_url(&url, &package.package_url).is_err()
            {
                invalid_entries = true;
                continue;
            }
            let identity = (
                package.manifest.id.clone(),
                package.manifest.version.clone(),
            );
            match identities.entry(identity) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Some(package));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    invalid_entries = true;
                    entry.insert(None);
                }
            }
        }
        Ok(FetchedIndex {
            packages: identities.into_values().flatten().collect(),
            invalid_entries,
        })
    }

    async fn fetch_bytes(&self, url: Url, limit: usize) -> Result<Vec<u8>, PluginError> {
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(PluginError::Network)?
            .error_for_status()
            .map_err(PluginError::Network)?;
        if !safe_network_scheme(response.url()) {
            return Err(PluginError::Verification(
                "download redirect violates the network policy".to_owned(),
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(PluginError::Verification(
                "download exceeds the size limit".to_owned(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(PluginError::Network)? {
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(PluginError::Verification(
                    "download exceeds the size limit".to_owned(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    fn checked_manifest(
        &self,
        installed: &InstalledPlugin,
        version: &str,
    ) -> Result<PluginManifest, PluginError> {
        let manifest = self.state.installed_manifest(&installed.id, version)?;
        let canonical = manifest
            .canonical_bytes()
            .map_err(PluginError::InvalidState)?;
        let digest = crate::operation::sha256(&canonical);
        let expected_digest = if version == installed.version {
            Some(installed.manifest_sha256.as_str())
        } else {
            installed
                .previous
                .as_ref()
                .filter(|retained| retained.version == version)
                .map(|retained| retained.manifest_sha256.as_str())
        };
        if expected_digest != Some(digest.as_str()) {
            return Err(PluginError::InvalidState(
                "installed manifest digest differs from the lockfile".to_owned(),
            ));
        }
        Ok(manifest)
    }
}

fn decode_registry_packages(
    packages: Vec<Box<serde_json::value::RawValue>>,
) -> (Vec<RegistryPackage>, bool) {
    let mut invalid_entries = false;
    let packages = packages
        .into_iter()
        .filter_map(|package| match serde_json::from_str(package.get()) {
            Ok(package) => Some(package),
            Err(_) => {
                invalid_entries = true;
                None
            }
        })
        .collect();
    (packages, invalid_entries)
}

fn reference_app(request: &PluginRequest) -> &str {
    match request {
        PluginRequest::Validate { app_id, .. }
        | PluginRequest::Import { app_id, .. }
        | PluginRequest::Current { app_id, .. } => app_id,
        PluginRequest::Plan { provider, .. } => &provider.app_id,
    }
}

fn registry_url(value: &str) -> Result<Url, PluginError> {
    let url = Url::parse(value)
        .map_err(|_| PluginError::Invalid("registry URL is invalid".to_owned()))?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !safe_network_scheme(&url)
    {
        return Err(PluginError::Invalid(
            "registry URL must be HTTPS or loopback HTTP without credentials, query, or fragment"
                .to_owned(),
        ));
    }
    Ok(url)
}

fn download_url(base: &Url, value: &str) -> Result<Url, PluginError> {
    let url = base
        .join(value)
        .map_err(|_| PluginError::Verification("package URL is invalid".to_owned()))?;
    if url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || !safe_network_scheme(&url)
    {
        return Err(PluginError::Verification(
            "package URL violates the network policy".to_owned(),
        ));
    }
    Ok(url)
}

fn safe_network_scheme(url: &Url) -> bool {
    if url.scheme() == "https" {
        return url.host().is_some();
    }
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".localhost")
        }
        None => false,
    }
}

fn redirect_allowed(next: &Url, previous: &[Url]) -> bool {
    safe_network_scheme(next)
        && !previous
            .last()
            .is_some_and(|url| url.scheme() == "https" && next.scheme() != "https")
}

fn version(value: &str) -> Version {
    Version::parse(value).expect("validated plugin manifest version")
}

fn catalog_candidate_is_better(
    candidate: &MarketplacePlugin,
    existing: &MarketplacePlugin,
) -> bool {
    let ownership = candidate.installed.as_ref().filter(|installed| {
        installed.registry_id == candidate.registry_id
            && installed.registry_id == existing.registry_id
    });
    if let Some(installed) = ownership {
        let candidate_matches = catalog_owner_matches(candidate, installed);
        let existing_matches = catalog_owner_matches(existing, installed);
        if candidate_matches != existing_matches {
            return candidate_matches;
        }
    }
    version(&candidate.manifest.version) > version(&existing.manifest.version)
}

fn catalog_owner_matches(candidate: &MarketplacePlugin, installed: &InstalledPlugin) -> bool {
    candidate.registry_id == installed.registry_id
        && candidate.manifest.publisher == installed.publisher
        && candidate.publisher_key_sha256 == installed.publisher_key_sha256
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{PublisherIdentity, SignatureAlgorithm};
    use std::collections::BTreeMap;

    fn catalog_plugin(
        version: &str,
        publisher: PublisherIdentity,
        key_digest: &str,
        installed: InstalledPlugin,
    ) -> MarketplacePlugin {
        MarketplacePlugin {
            registry_id: "registry-a".to_owned(),
            registry_revision: 1,
            registry_label: "Registry A".to_owned(),
            manifest: PluginManifest {
                schema_version: 1,
                id: installed.id.clone(),
                version: version.to_owned(),
                host_api_major: 1,
                name: "Fixture".to_owned(),
                description: String::new(),
                publisher,
                component: "plugin.wasm".to_owned(),
                adapters: Vec::new(),
                capabilities: Vec::new(),
                files: BTreeMap::new(),
            },
            manifest_sha256: "a".repeat(64),
            package_sha256: "b".repeat(64),
            publisher_key_sha256: key_digest.to_owned(),
            installed: Some(installed),
            permissions: Vec::new(),
        }
    }

    #[test]
    fn registry_network_policy_allows_https_and_loopback_only() {
        assert!(registry_url("https://plugins.example.com/index.json").is_ok());
        assert!(registry_url("http://localhost:8080/index.json").is_ok());
        assert!(registry_url("http://127.0.0.1:8080/index.json").is_ok());
        assert!(registry_url("http://plugins.example.com/index.json").is_err());
        assert!(registry_url("file:///tmp/index.json").is_err());
        assert!(registry_url("https://user:secret@plugins.example.com/index.json").is_err());
        assert!(registry_url("https://plugins.example.com/index.json?token=secret").is_err());
    }

    #[test]
    fn package_urls_cannot_downgrade_or_escape_the_network_policy() {
        let base = registry_url("https://plugins.example.com/index.json").unwrap();

        assert_eq!(
            download_url(&base, "releases/plugin.zip").unwrap().as_str(),
            "https://plugins.example.com/releases/plugin.zip"
        );
        assert!(download_url(&base, "http://plugins.example.com/plugin.zip").is_err());
        assert!(download_url(&base, "file:///tmp/plugin.zip").is_err());
    }

    #[test]
    fn redirects_are_checked_before_each_request() {
        let remote_https = Url::parse("https://plugins.example.com/index.json").unwrap();
        let other_https = Url::parse("https://cdn.example.com/index.json").unwrap();
        let loopback_http = Url::parse("http://127.0.0.1:8080/index.json").unwrap();
        let remote_http = Url::parse("http://169.254.169.254/index.json").unwrap();

        assert!(redirect_allowed(
            &other_https,
            std::slice::from_ref(&remote_https)
        ));
        assert!(!redirect_allowed(
            &loopback_http,
            std::slice::from_ref(&remote_https)
        ));
        assert!(!redirect_allowed(
            &remote_http,
            std::slice::from_ref(&remote_https)
        ));
        assert!(redirect_allowed(
            &other_https,
            std::slice::from_ref(&loopback_http)
        ));
    }

    #[test]
    fn catalog_prefers_the_installed_ownership_chain_over_a_higher_collision() {
        let publisher = PublisherIdentity {
            id: "dev.owner".to_owned(),
            key_id: "release".to_owned(),
            algorithm: SignatureAlgorithm::Ed25519,
        };
        let installed = InstalledPlugin {
            id: "dev.owner.plugin".to_owned(),
            version: "1.0.0".to_owned(),
            previous: None,
            registry_id: "registry-a".to_owned(),
            package_sha256: "a".repeat(64),
            manifest_sha256: "b".repeat(64),
            publisher: publisher.clone(),
            publisher_key_sha256: "c".repeat(64),
            granted_capabilities: Vec::new(),
        };
        let owned = catalog_plugin("1.1.0", publisher, &"c".repeat(64), installed.clone());
        let collision = catalog_plugin(
            "2.0.0",
            PublisherIdentity {
                id: "dev.other".to_owned(),
                key_id: "release".to_owned(),
                algorithm: SignatureAlgorithm::Ed25519,
            },
            &"d".repeat(64),
            installed,
        );

        assert!(catalog_candidate_is_better(&owned, &collision));
        assert!(!catalog_candidate_is_better(&collision, &owned));
    }

    #[test]
    fn structurally_invalid_packages_do_not_hide_valid_entries() {
        let package = RegistryPackage {
            manifest: PluginManifest {
                schema_version: 1,
                id: "dev.owner.plugin".to_owned(),
                version: "1.0.0".to_owned(),
                host_api_major: 1,
                name: "Fixture".to_owned(),
                description: String::new(),
                publisher: PublisherIdentity {
                    id: "dev.owner".to_owned(),
                    key_id: "release".to_owned(),
                    algorithm: SignatureAlgorithm::Ed25519,
                },
                component: "plugin.wasm".to_owned(),
                adapters: Vec::new(),
                capabilities: Vec::new(),
                files: BTreeMap::new(),
            },
            manifest_sha256: "a".repeat(64),
            signature: "signature".to_owned(),
            package_url: "plugin.zip".to_owned(),
            package_sha256: "b".repeat(64),
        };

        let (packages, invalid_entries) = decode_registry_packages(vec![
            serde_json::value::to_raw_value(&package).unwrap(),
            serde_json::value::RawValue::from_string("{}".to_owned()).unwrap(),
        ]);

        assert!(invalid_entries);
        assert_eq!(packages, vec![package]);
    }
}
