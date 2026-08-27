use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};

use cc_switch_core::fs::atomic_write_private;
use fs4::{FileExt, TryLockError};
use semver::Version;

use super::{
    io_error, package::verify_installed_directory, validate_dotted_id, InstalledFile,
    InstalledPlugin, PluginError, PluginManifest, RegistryDraft, RegistryFile, RegistrySource,
    RetainedPluginVersion, INSTALLED_SCHEMA_VERSION, REGISTRY_STORE_VERSION,
};

const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REGISTRIES: usize = 16;
const STALE_STAGING_AGE: Duration = Duration::from_secs(60 * 60);

pub(super) struct Activation<'a> {
    pub staging: &'a Path,
    pub registry: &'a RegistrySource,
    pub manifest: &'a PluginManifest,
    pub package_sha256: &'a str,
    pub manifest_sha256: &'a str,
    pub signature: &'a str,
    pub publisher_key_sha256: &'a str,
    pub plugin_in_use: bool,
}

pub struct PluginState {
    root: PathBuf,
    registry_path: PathBuf,
    installed_path: PathBuf,
    lock_path: PathBuf,
    gate: Mutex<()>,
}

impl PluginState {
    pub fn new(root: PathBuf) -> Self {
        Self {
            registry_path: root.join("registries.json"),
            installed_path: root.join("installed.json"),
            lock_path: root.join("plugins.lock"),
            root,
            gate: Mutex::new(()),
        }
    }

    pub fn recover(&self) -> Result<(), PluginError> {
        self.with_recovery_lock(|| Ok(()))
    }

    pub fn list_registries(&self) -> Result<Vec<RegistrySource>, PluginError> {
        self.with_lock(|| Ok(self.read_registries()?.registries))
    }

    pub fn registry(&self, id: &str) -> Result<RegistrySource, PluginError> {
        self.with_lock(|| {
            self.read_registries()?
                .registries
                .into_iter()
                .find(|registry| registry.id == id)
                .ok_or(PluginError::NotFound)
        })
    }

    pub fn save_registry(
        &self,
        draft: RegistryDraft,
        normalized_url: String,
    ) -> Result<RegistrySource, PluginError> {
        self.with_lock(|| {
            let mut file = self.read_registries()?;
            let label = draft.label.trim();
            if label.is_empty() || label.chars().count() > 80 {
                return Err(PluginError::Invalid(
                    "registry label must contain at most 80 characters".to_owned(),
                ));
            }
            if draft.trusted_publishers.is_empty() || draft.trusted_publishers.len() > 64 {
                return Err(PluginError::Invalid(
                    "a registry must trust between 1 and 64 publisher keys".to_owned(),
                ));
            }

            let saved = match draft.id {
                Some(id) => {
                    let registry = file
                        .registries
                        .iter_mut()
                        .find(|registry| registry.id == id)
                        .ok_or(PluginError::NotFound)?;
                    if Some(registry.revision) != draft.expected_revision {
                        return Err(PluginError::Conflict);
                    }
                    registry.revision = registry
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| PluginError::InvalidState("revision overflow".to_owned()))?;
                    registry.label = label.to_owned();
                    registry.index_url = normalized_url;
                    registry.enabled = draft.enabled;
                    registry.trusted_publishers = draft.trusted_publishers;
                    registry.clone()
                }
                None => {
                    if draft.expected_revision.is_some() {
                        return Err(PluginError::Invalid(
                            "a new registry cannot have an expected revision".to_owned(),
                        ));
                    }
                    if file.registries.len() >= MAX_REGISTRIES {
                        return Err(PluginError::Invalid(format!(
                            "at most {MAX_REGISTRIES} registries can be configured"
                        )));
                    }
                    let registry = RegistrySource {
                        id: uuid::Uuid::new_v4().to_string(),
                        revision: 1,
                        label: label.to_owned(),
                        index_url: normalized_url,
                        enabled: draft.enabled,
                        trusted_publishers: draft.trusted_publishers,
                    };
                    file.registries.push(registry.clone());
                    registry
                }
            };
            self.write_json(&self.registry_path, &file)?;
            Ok(saved)
        })
    }

    pub fn remove_registry(&self, id: &str, expected_revision: u64) -> Result<(), PluginError> {
        self.with_lock(|| {
            let mut file = self.read_registries()?;
            let index = file
                .registries
                .iter()
                .position(|registry| registry.id == id)
                .ok_or(PluginError::NotFound)?;
            if file.registries[index].revision != expected_revision {
                return Err(PluginError::Conflict);
            }
            file.registries.remove(index);
            self.write_json(&self.registry_path, &file)
        })
    }

    pub fn list_installed(&self) -> Result<Vec<InstalledPlugin>, PluginError> {
        self.with_recovery_lock(|| Ok(self.read_installed()?.plugins))
    }

    pub fn installed(&self, plugin_id: &str) -> Result<InstalledPlugin, PluginError> {
        self.with_recovery_lock(|| {
            self.read_installed()?
                .plugins
                .into_iter()
                .find(|plugin| plugin.id == plugin_id)
                .ok_or(PluginError::NotFound)
        })
    }

    pub fn installed_manifest(
        &self,
        plugin_id: &str,
        version: &str,
    ) -> Result<PluginManifest, PluginError> {
        validate_dotted_id(plugin_id, "plugin ID").map_err(PluginError::Invalid)?;
        Version::parse(version)
            .map_err(|_| PluginError::Invalid("plugin version is not semantic".to_owned()))?;
        let path = self.version_path(plugin_id, version).join("manifest.json");
        let bytes = read_bounded(&path)?;
        let manifest: PluginManifest = serde_json::from_slice(&bytes)
            .map_err(|_| PluginError::InvalidState("installed manifest is invalid".to_owned()))?;
        manifest.validate().map_err(PluginError::InvalidState)?;
        if manifest.id != plugin_id || manifest.version != version {
            return Err(PluginError::InvalidState(
                "installed manifest identity does not match its directory".to_owned(),
            ));
        }
        Ok(manifest)
    }

    pub fn component_path(&self, manifest: &PluginManifest) -> PathBuf {
        self.version_path(&manifest.id, &manifest.version)
            .join(&manifest.component)
    }

    pub fn activate(&self, activation: Activation<'_>) -> Result<InstalledPlugin, PluginError> {
        let Activation {
            staging,
            registry,
            manifest,
            package_sha256,
            manifest_sha256,
            signature,
            publisher_key_sha256,
            plugin_in_use,
        } = activation;
        self.with_recovery_lock(|| {
            let current_registry = self
                .read_registries()?
                .registries
                .into_iter()
                .find(|candidate| candidate.id == registry.id)
                .ok_or(PluginError::Conflict)?;
            if current_registry != *registry {
                return Err(PluginError::Conflict);
            }

            let destination = self.version_path(&manifest.id, &manifest.version);
            let mut installed = self.read_installed()?;
            let existing = installed
                .plugins
                .iter()
                .find(|plugin| plugin.id == manifest.id)
                .cloned();
            if existing.as_ref().is_some_and(|plugin| {
                plugin.registry_id != registry.id
                    || plugin.publisher != manifest.publisher
                    || plugin.publisher_key_sha256 != publisher_key_sha256
            }) {
                return Err(PluginError::Invalid(
                    "plugin ID is owned by a different source or publisher key".to_owned(),
                ));
            }
            if plugin_in_use
                && existing.as_ref().is_some_and(|plugin| {
                    plugin.version != manifest.version || plugin.manifest_sha256 != manifest_sha256
                })
            {
                return Err(PluginError::Invalid(
                    "remove providers owned by the active plugin before updating it".to_owned(),
                ));
            }

            if destination.exists() {
                let existing_manifest = self.installed_manifest(&manifest.id, &manifest.version)?;
                if existing_manifest
                    .canonical_bytes()
                    .map_err(PluginError::InvalidState)?
                    != manifest.canonical_bytes().map_err(PluginError::Invalid)?
                {
                    return Err(PluginError::Verification(
                        "an installed version has different signed contents".to_owned(),
                    ));
                }
                verify_installed_directory(&destination, manifest, signature.as_bytes())?;
                fs::remove_dir_all(staging).map_err(|source| io_error(staging, source))?;
            } else {
                if let Some(parent) = destination.parent() {
                    create_private_dir(parent)?;
                }
                fs::rename(staging, &destination)
                    .map_err(|source| io_error(&destination, source))?;
            }

            let record = InstalledPlugin {
                id: manifest.id.clone(),
                version: manifest.version.clone(),
                previous: existing
                    .as_ref()
                    .filter(|plugin| plugin.version != manifest.version)
                    .map(|plugin| RetainedPluginVersion {
                        version: plugin.version.clone(),
                        package_sha256: plugin.package_sha256.clone(),
                        manifest_sha256: plugin.manifest_sha256.clone(),
                        publisher: plugin.publisher.clone(),
                        publisher_key_sha256: plugin.publisher_key_sha256.clone(),
                    })
                    .or_else(|| existing.as_ref().and_then(|plugin| plugin.previous.clone())),
                registry_id: registry.id.clone(),
                package_sha256: package_sha256.to_owned(),
                manifest_sha256: manifest_sha256.to_owned(),
                publisher: manifest.publisher.clone(),
                publisher_key_sha256: publisher_key_sha256.to_owned(),
                granted_capabilities: manifest.capabilities.clone(),
            };
            installed.plugins.retain(|plugin| plugin.id != manifest.id);
            installed.plugins.push(record.clone());
            installed
                .plugins
                .sort_by(|left, right| left.id.cmp(&right.id));
            self.write_json(&self.installed_path, &installed)?;
            let _ = self.cleanup_versions(&record);
            Ok(record)
        })
    }

    pub fn remove_installed(&self, plugin_id: &str) -> Result<(), PluginError> {
        validate_dotted_id(plugin_id, "plugin ID").map_err(PluginError::Invalid)?;
        self.with_recovery_lock(|| {
            let mut installed = self.read_installed()?;
            let index = installed
                .plugins
                .iter()
                .position(|plugin| plugin.id == plugin_id)
                .ok_or(PluginError::NotFound)?;
            let directory = self.versions_root().join(plugin_id);
            let tombstone = match fs::symlink_metadata(&directory) {
                Ok(_) => {
                    let trash = self.root.join("trash");
                    create_private_dir(&trash)?;
                    let tombstone = trash.join(plugin_id);
                    remove_path(&tombstone)?;
                    fs::rename(&directory, &tombstone)
                        .map_err(|source| io_error(&directory, source))?;
                    Some(tombstone)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => return Err(io_error(&directory, source)),
            };

            installed.plugins.remove(index);
            if let Err(error) = self.write_json(&self.installed_path, &installed) {
                if let Some(tombstone) = &tombstone {
                    if let Err(source) = fs::rename(tombstone, &directory) {
                        return Err(PluginError::InvalidState(format!(
                            "uninstall state write failed and plugin directory rollback failed: {source}"
                        )));
                    }
                }
                return Err(error);
            }
            if let Some(tombstone) = tombstone {
                let _ = remove_path(&tombstone);
            }
            Ok(())
        })
    }

    pub fn staging_path(&self) -> Result<PathBuf, PluginError> {
        let root = self.root.join("staging");
        create_private_dir(&root)?;
        Ok(root.join(uuid::Uuid::new_v4().to_string()))
    }

    fn versions_root(&self) -> PathBuf {
        self.root.join("versions")
    }

    fn version_path(&self, plugin_id: &str, version: &str) -> PathBuf {
        self.versions_root().join(plugin_id).join(version)
    }

    fn cleanup_versions(&self, active: &InstalledPlugin) -> Result<(), PluginError> {
        let root = self.versions_root().join(&active.id);
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(&root, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&root, source))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == active.version
                || active
                    .previous
                    .as_ref()
                    .map(|version| version.version.as_str())
                    == Some(&name)
            {
                continue;
            }
            if Version::parse(&name).is_err() {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
            if metadata.file_type().is_symlink() {
                fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
            } else if metadata.is_dir() {
                fs::remove_dir_all(&path).map_err(|source| io_error(&path, source))?;
            }
        }
        Ok(())
    }

    fn recover_trash(&self, installed: &InstalledFile) -> Result<(), PluginError> {
        let trash = self.root.join("trash");
        let entries = match fs::read_dir(&trash) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(&trash, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&trash, source))?;
            let path = entry.path();
            let Some(plugin_id) = entry.file_name().to_str().map(str::to_owned) else {
                remove_path(&path)?;
                continue;
            };
            let destination = self.versions_root().join(&plugin_id);
            if installed
                .plugins
                .iter()
                .any(|plugin| plugin.id == plugin_id)
                && !destination.exists()
            {
                create_private_dir(&self.versions_root())?;
                fs::rename(&path, &destination).map_err(|source| io_error(&destination, source))?;
            } else {
                remove_path(&path)?;
            }
        }
        Ok(())
    }

    fn clean_stale_staging(&self) -> Result<(), PluginError> {
        let directory = self.root.join("staging");
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(directory, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&directory, source))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
            let stale = metadata.modified().is_ok_and(|modified| {
                SystemTime::now()
                    .duration_since(modified)
                    .is_ok_and(|age| age >= STALE_STAGING_AGE)
            });
            if stale {
                remove_path(&path)?;
            }
        }
        Ok(())
    }

    fn cleanup_inert_versions(&self, installed: &InstalledFile) -> Result<(), PluginError> {
        let root = self.versions_root();
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(&root, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| io_error(&root, source))?;
            let path = entry.path();
            let Some(plugin_id) = entry.file_name().to_str().map(str::to_owned) else {
                let _ = remove_path(&path);
                continue;
            };
            let Some(active) = installed
                .plugins
                .iter()
                .find(|plugin| plugin.id == plugin_id)
            else {
                let _ = remove_path(&path);
                continue;
            };
            // Recovery is best-effort per plugin so one damaged root cannot hide healthy plugins.
            let _ = self.cleanup_plugin_version_root(&path, active);
        }
        Ok(())
    }

    fn cleanup_plugin_version_root(
        &self,
        path: &Path,
        active: &InstalledPlugin,
    ) -> Result<(), PluginError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return remove_path(path);
        }
        let versions = fs::read_dir(path).map_err(|source| io_error(path, source))?;
        for version in versions {
            let version = version.map_err(|source| io_error(path, source))?;
            let name = version.file_name().to_string_lossy().into_owned();
            let retained = name == active.version
                || active
                    .previous
                    .as_ref()
                    .is_some_and(|previous| previous.version == name);
            if !retained {
                remove_path(&version.path())?;
            }
        }
        Ok(())
    }

    fn read_registries(&self) -> Result<RegistryFile, PluginError> {
        let file: RegistryFile = read_json_or_default(&self.registry_path)?;
        if file.version != REGISTRY_STORE_VERSION {
            return Err(PluginError::InvalidState(
                "unsupported registry store version".to_owned(),
            ));
        }
        Ok(file)
    }

    fn read_installed(&self) -> Result<InstalledFile, PluginError> {
        let file: InstalledFile = read_json_or_default(&self.installed_path)?;
        if file.version != INSTALLED_SCHEMA_VERSION {
            return Err(PluginError::InvalidState(
                "unsupported installed plugin state version".to_owned(),
            ));
        }
        Ok(file)
    }

    fn write_json<T: serde::Serialize>(&self, path: &Path, value: &T) -> Result<(), PluginError> {
        let contents = serde_json::to_vec_pretty(value).map_err(PluginError::Serialize)?;
        if contents.len() as u64 > MAX_STATE_BYTES {
            return Err(PluginError::InvalidState(
                "plugin state exceeds the size limit".to_owned(),
            ));
        }
        atomic_write_private(path, &contents)
            .map_err(|error| PluginError::InvalidState(error.to_string()))
    }

    fn with_lock<T>(
        &self,
        action: impl FnOnce() -> Result<T, PluginError>,
    ) -> Result<T, PluginError> {
        self.with_lock_mode(false, action)
    }

    fn with_recovery_lock<T>(
        &self,
        action: impl FnOnce() -> Result<T, PluginError>,
    ) -> Result<T, PluginError> {
        self.with_lock_mode(true, action)
    }

    fn with_lock_mode<T>(
        &self,
        recover: bool,
        action: impl FnOnce() -> Result<T, PluginError>,
    ) -> Result<T, PluginError> {
        let _guard = self
            .gate
            .try_lock()
            .map_err(|_| PluginError::LockUnavailable)?;
        let _file = self.lock_file()?;
        if recover {
            let installed = self.read_installed()?;
            self.recover_trash(&installed)?;
            self.clean_stale_staging()?;
            self.cleanup_inert_versions(&installed)?;
        }
        action()
    }

    fn lock_file(&self) -> Result<File, PluginError> {
        create_private_dir(&self.root)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&self.lock_path)
            .map_err(|source| io_error(&self.lock_path, source))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            lock.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|source| io_error(&self.lock_path, source))?;
        }
        FileExt::try_lock(&lock).map_err(|error| match error {
            TryLockError::WouldBlock => PluginError::LockUnavailable,
            TryLockError::Error(source) => io_error(&self.lock_path, source),
        })?;
        Ok(lock)
    }
}

fn read_json_or_default<T>(path: &Path) -> Result<T, PluginError>
where
    T: serde::de::DeserializeOwned + Default,
{
    match read_bounded(path) {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|_| PluginError::InvalidState("plugin state JSON is invalid".to_owned())),
        Err(PluginError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(T::default())
        }
        Err(error) => Err(error),
    }
}

fn remove_path(path: &Path) -> Result<(), PluginError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(path).map_err(|source| io_error(path, source))
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path).map_err(|source| io_error(path, source))
        }
        Ok(_) => Err(PluginError::InvalidState(
            "plugin cleanup path is not removable".to_owned(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(path, source)),
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, PluginError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PluginError::InvalidState(
            "plugin state path is not a regular file".to_owned(),
        ));
    }
    if metadata.len() > MAX_STATE_BYTES {
        return Err(PluginError::InvalidState(
            "plugin state exceeds the size limit".to_owned(),
        ));
    }
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|source| io_error(path, source))?;
    if contents.len() as u64 > MAX_STATE_BYTES {
        return Err(PluginError::InvalidState(
            "plugin state exceeds the size limit".to_owned(),
        ));
    }
    Ok(contents)
}

pub fn create_private_dir(path: &Path) -> Result<(), PluginError> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error(path, source))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{
        PublisherIdentity, SignatureAlgorithm, COMPONENT_PATH, MANIFEST_SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;

    fn publisher() -> PublisherIdentity {
        PublisherIdentity {
            id: "dev.example".to_owned(),
            key_id: "release-1".to_owned(),
            algorithm: SignatureAlgorithm::Ed25519,
        }
    }

    fn installed(id: &str, version: &str) -> InstalledPlugin {
        InstalledPlugin {
            id: id.to_owned(),
            version: version.to_owned(),
            previous: None,
            registry_id: "registry-a".to_owned(),
            package_sha256: "a".repeat(64),
            manifest_sha256: "b".repeat(64),
            publisher: publisher(),
            publisher_key_sha256: "c".repeat(64),
            granted_capabilities: Vec::new(),
        }
    }

    fn manifest(version: &str) -> PluginManifest {
        PluginManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            id: "dev.example.fixture".to_owned(),
            version: version.to_owned(),
            host_api_major: 1,
            name: "Fixture".to_owned(),
            description: "Fixture".to_owned(),
            publisher: publisher(),
            component: COMPONENT_PATH.to_owned(),
            adapters: Vec::new(),
            capabilities: Vec::new(),
            files: BTreeMap::new(),
        }
    }

    fn registry(id: &str) -> RegistrySource {
        RegistrySource {
            id: id.to_owned(),
            revision: 1,
            label: id.to_owned(),
            index_url: "https://plugins.example/index.json".to_owned(),
            enabled: true,
            trusted_publishers: Vec::new(),
        }
    }

    #[test]
    fn every_locked_read_recovers_an_interrupted_uninstall_and_inert_versions() {
        let directory = tempfile::tempdir().unwrap();
        let state = PluginState::new(directory.path().join("plugins"));
        create_private_dir(&state.root).unwrap();
        let record = installed("dev.example.fixture", "1.0.0");
        state
            .write_json(
                &state.installed_path,
                &InstalledFile {
                    version: INSTALLED_SCHEMA_VERSION,
                    plugins: vec![record],
                },
            )
            .unwrap();
        let tombstone = state.root.join("trash/dev.example.fixture/1.0.0");
        create_private_dir(&tombstone).unwrap();
        fs::write(tombstone.join("marker"), "active").unwrap();

        state.list_installed().unwrap();

        let active = state.version_path("dev.example.fixture", "1.0.0");
        assert_eq!(fs::read_to_string(active.join("marker")).unwrap(), "active");
        let inert = state.version_path("dev.example.fixture", "2.0.0");
        create_private_dir(&inert).unwrap();
        state.list_installed().unwrap();
        assert!(!inert.exists());
        assert!(active.exists());
    }

    #[test]
    fn damaged_plugin_roots_do_not_block_recovery_or_healthy_entries() {
        let directory = tempfile::tempdir().unwrap();
        let state = PluginState::new(directory.path().join("plugins"));
        create_private_dir(&state.root).unwrap();
        state
            .write_json(
                &state.installed_path,
                &InstalledFile {
                    version: INSTALLED_SCHEMA_VERSION,
                    plugins: vec![
                        installed("dev.example.file", "1.0.0"),
                        installed("dev.example.healthy", "1.0.0"),
                        #[cfg(unix)]
                        installed("dev.example.link", "1.0.0"),
                    ],
                },
            )
            .unwrap();

        create_private_dir(&state.versions_root()).unwrap();
        let damaged_file = state.versions_root().join("dev.example.file");
        fs::write(&damaged_file, "damaged").unwrap();
        let healthy = state.version_path("dev.example.healthy", "1.0.0");
        create_private_dir(&healthy).unwrap();
        #[cfg(unix)]
        let damaged_link = {
            use std::os::unix::fs::symlink;

            let target = directory.path().join("link-target");
            create_private_dir(&target).unwrap();
            let link = state.versions_root().join("dev.example.link");
            symlink(&target, &link).unwrap();
            link
        };

        state.recover().unwrap();

        let installed = state.list_installed().unwrap();
        assert_eq!(installed.len(), if cfg!(unix) { 3 } else { 2 });
        assert!(healthy.exists());
        assert!(fs::symlink_metadata(&damaged_file).is_err());
        #[cfg(unix)]
        assert!(fs::symlink_metadata(&damaged_link).is_err());
    }

    #[test]
    fn activation_rejects_a_different_source_and_an_in_use_manifest_change() {
        let directory = tempfile::tempdir().unwrap();
        let state = PluginState::new(directory.path().join("plugins"));
        create_private_dir(&state.root).unwrap();
        let registry_a = registry("registry-a");
        let registry_b = registry("registry-b");
        state
            .write_json(
                &state.registry_path,
                &RegistryFile {
                    version: REGISTRY_STORE_VERSION,
                    registries: vec![registry_a.clone(), registry_b.clone()],
                },
            )
            .unwrap();
        let first_staging = state.root.join("first-staging");
        create_private_dir(&first_staging).unwrap();
        state
            .activate(Activation {
                staging: &first_staging,
                registry: &registry_a,
                manifest: &manifest("1.0.0"),
                package_sha256: &"a".repeat(64),
                manifest_sha256: &"b".repeat(64),
                signature: "signature",
                publisher_key_sha256: &"c".repeat(64),
                plugin_in_use: false,
            })
            .unwrap();

        let other_staging = state.root.join("other-staging");
        create_private_dir(&other_staging).unwrap();
        assert!(matches!(
            state.activate(Activation {
                staging: &other_staging,
                registry: &registry_b,
                manifest: &manifest("2.0.0"),
                package_sha256: &"d".repeat(64),
                manifest_sha256: &"e".repeat(64),
                signature: "signature",
                publisher_key_sha256: &"f".repeat(64),
                plugin_in_use: false,
            }),
            Err(PluginError::Invalid(_))
        ));

        assert!(matches!(
            state.activate(Activation {
                staging: &other_staging,
                registry: &registry_a,
                manifest: &manifest("1.0.0"),
                package_sha256: &"a".repeat(64),
                manifest_sha256: &"e".repeat(64),
                signature: "signature",
                publisher_key_sha256: &"c".repeat(64),
                plugin_in_use: true,
            }),
            Err(PluginError::Invalid(_))
        ));
    }
}
