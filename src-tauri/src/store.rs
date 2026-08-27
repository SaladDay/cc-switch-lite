use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::Mutex,
};

use cc_switch_core::fs::{atomic_write_private, read_json_file, FileError};
use cc_switch_core::AppType;
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::provider::{
    adapter_for_reference, validate_name, validate_settings, ProviderDraft, ProviderRecord,
    ProviderUpdate,
};

const STORE_VERSION: u32 = 1;
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    File(#[from] FileError),
    #[error("provider store serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("provider store is invalid: {0}")]
    InvalidStore(String),
    #[error("provider is invalid: {0}")]
    InvalidProvider(String),
    #[error("provider '{0}' was not found")]
    NotFound(String),
    #[error("provider store lock is unavailable")]
    LockUnavailable,
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::File(_) | Self::Serialize(_) => "storage_error",
            Self::InvalidStore(_) => "invalid_store",
            Self::InvalidProvider(_) => "invalid_provider",
            Self::NotFound(_) => "not_found",
            Self::LockUnavailable => "lock_unavailable",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderFile {
    version: u32,
    providers: Vec<ProviderRecord>,
}

impl Default for ProviderFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            providers: Vec::new(),
        }
    }
}

pub struct ProviderStore {
    path: PathBuf,
    lock_path: PathBuf,
    gate: Mutex<()>,
}

impl ProviderStore {
    pub fn new(path: PathBuf) -> Self {
        let lock_path = path.with_extension("lock");
        Self {
            path,
            lock_path,
            gate: Mutex::new(()),
        }
    }

    pub fn list(&self, app_id: &str) -> Result<Vec<ProviderRecord>, StoreError> {
        ensure_supported_app(app_id)?;
        let _guard = self.gate.lock().map_err(|_| StoreError::LockUnavailable)?;
        let _file_lock = self.lock_file()?;
        let file = self.read()?;
        Ok(file
            .providers
            .into_iter()
            .filter(|provider| provider.app_id == app_id)
            .collect())
    }

    pub fn create(&self, draft: ProviderDraft) -> Result<ProviderRecord, StoreError> {
        let descriptor = adapter_for_reference(&draft.app_id, &draft.adapter).ok_or_else(|| {
            StoreError::InvalidProvider("the selected adapter is not available".to_owned())
        })?;
        let name = validate_name(&draft.name).map_err(StoreError::InvalidProvider)?;
        validate_settings(&descriptor, &draft.settings).map_err(StoreError::InvalidProvider)?;

        let _guard = self.gate.lock().map_err(|_| StoreError::LockUnavailable)?;
        let _file_lock = self.lock_file()?;
        let mut file = self.read()?;
        let provider = ProviderRecord {
            id: Uuid::new_v4().to_string(),
            app_id: descriptor.app_id,
            adapter: descriptor.reference,
            name,
            settings: draft.settings,
            extensions: serde_json::Map::new(),
        };
        file.providers.push(provider.clone());
        self.write(&file)?;
        Ok(provider)
    }

    pub fn update(&self, id: &str, update: ProviderUpdate) -> Result<ProviderRecord, StoreError> {
        let name = validate_name(&update.name).map_err(StoreError::InvalidProvider)?;
        let _guard = self.gate.lock().map_err(|_| StoreError::LockUnavailable)?;
        let _file_lock = self.lock_file()?;
        let mut file = self.read()?;
        let provider = file
            .providers
            .iter_mut()
            .find(|provider| provider.id == id)
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let descriptor =
            adapter_for_reference(&provider.app_id, &provider.adapter).ok_or_else(|| {
                StoreError::InvalidProvider("the provider adapter is unavailable".to_owned())
            })?;
        validate_settings(&descriptor, &update.settings).map_err(StoreError::InvalidProvider)?;

        provider.name = name;
        provider.settings = update.settings;
        let updated = provider.clone();
        self.write(&file)?;
        Ok(updated)
    }

    pub fn delete(&self, app_id: &str, id: &str) -> Result<(), StoreError> {
        ensure_supported_app(app_id)?;
        let _guard = self.gate.lock().map_err(|_| StoreError::LockUnavailable)?;
        let _file_lock = self.lock_file()?;
        let mut file = self.read()?;
        let index = file
            .providers
            .iter()
            .position(|provider| provider.id == id && provider.app_id == app_id)
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        file.providers.remove(index);
        self.write(&file)
    }

    fn lock_file(&self) -> Result<File, StoreError> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| file_error(parent, source))?;
        }

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&self.lock_path)
            .map_err(|source| file_error(&self.lock_path, source))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            lock.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|source| file_error(&self.lock_path, source))?;
        }

        FileExt::lock(&lock).map_err(|source| file_error(&self.lock_path, source))?;
        Ok(lock)
    }

    fn read(&self) -> Result<ProviderFile, StoreError> {
        match fs::metadata(&self.path) {
            Ok(metadata) if metadata.len() > MAX_STORE_BYTES => {
                return Err(StoreError::InvalidStore(format!(
                    "file exceeds the {MAX_STORE_BYTES} byte limit"
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ProviderFile::default());
            }
            Err(error) => {
                return Err(StoreError::File(FileError::Io {
                    path: self.path.clone(),
                    source: error,
                }));
            }
        }

        let file: ProviderFile = read_json_file(&self.path)?;
        validate_file(&file)?;
        Ok(file)
    }

    fn write(&self, file: &ProviderFile) -> Result<(), StoreError> {
        let contents = serde_json::to_vec_pretty(file)?;
        if contents.len() as u64 > MAX_STORE_BYTES {
            return Err(StoreError::InvalidStore(format!(
                "file exceeds the {MAX_STORE_BYTES} byte limit"
            )));
        }
        atomic_write_private(&self.path, &contents)?;
        Ok(())
    }
}

fn file_error(path: &Path, source: std::io::Error) -> StoreError {
    StoreError::File(FileError::Io {
        path: path.to_owned(),
        source,
    })
}

fn ensure_supported_app(app_id: &str) -> Result<(), StoreError> {
    match app_id.parse::<AppType>() {
        Ok(AppType::Claude | AppType::Codex) => Ok(()),
        _ => Err(StoreError::InvalidProvider(format!(
            "application '{app_id}' is not available in Lite"
        ))),
    }
}

fn validate_file(file: &ProviderFile) -> Result<(), StoreError> {
    if file.version != STORE_VERSION {
        return Err(StoreError::InvalidStore(format!(
            "unsupported version {}",
            file.version
        )));
    }

    let mut ids = HashSet::new();
    for provider in &file.providers {
        if provider.id.is_empty()
            || provider.app_id.is_empty()
            || provider.adapter.plugin_id.is_empty()
            || provider.adapter.plugin_version.is_empty()
            || provider.adapter.adapter_id.is_empty()
            || provider.adapter.contract_major == 0
            || provider.adapter.schema_version == 0
            || provider.name.trim().is_empty()
        {
            return Err(StoreError::InvalidStore(
                "a provider record is incomplete".to_owned(),
            ));
        }
        if !ids.insert(&provider.id) {
            return Err(StoreError::InvalidStore(format!(
                "duplicate provider id '{}'",
                provider.id
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::built_in_adapters;
    use serde_json::{json, Map};
    use std::{sync::mpsc, thread, time::Duration};

    fn settings(value: serde_json::Value) -> Map<String, serde_json::Value> {
        value.as_object().expect("settings object").clone()
    }

    fn draft(app_id: &str, adapter_id: &str, name: &str) -> ProviderDraft {
        let adapter = built_in_adapters()
            .into_iter()
            .find(|adapter| adapter.reference.adapter_id == adapter_id)
            .expect("built-in adapter")
            .reference;
        ProviderDraft {
            app_id: app_id.to_owned(),
            adapter,
            name: name.to_owned(),
            settings: settings(json!({"apiKey": "secret"})),
        }
    }

    #[test]
    fn create_and_list_persist_a_private_provider_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        let store = ProviderStore::new(path.clone());

        let created = store
            .create(draft("claude", "builtin.claude.api-key", "  Work  "))
            .expect("create provider");
        let reopened = ProviderStore::new(path.clone());

        assert_eq!(created.name, "Work");
        assert_eq!(reopened.list("claude").expect("list providers"), [created]);
        assert!(reopened.list("codex").expect("list providers").is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn update_and_delete_preserve_the_provider_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ProviderStore::new(directory.path().join("providers.json"));
        let created = store
            .create(draft("codex", "builtin.codex.api-key", "Personal"))
            .expect("create provider");

        let updated = store
            .update(
                &created.id,
                ProviderUpdate {
                    name: "Primary".to_owned(),
                    settings: settings(json!({
                        "apiKey": "new-secret",
                        "baseUrl": "http://localhost:8080"
                    })),
                },
            )
            .expect("update provider");

        assert_eq!(updated.id, created.id);
        assert_eq!(updated.adapter, created.adapter);
        assert_eq!(updated.name, "Primary");
        store.delete("codex", &created.id).expect("delete provider");
        assert!(store.list("codex").expect("list providers").is_empty());
    }

    #[test]
    fn invalid_drafts_do_not_create_the_store() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        let store = ProviderStore::new(path.clone());
        let result = store.create(draft("gemini", "builtin.claude.api-key", "Invalid"));

        assert!(matches!(result, Err(StoreError::InvalidProvider(_))));
        assert!(!path.exists());
    }

    #[test]
    fn unknown_plugin_fields_survive_builtin_mutations() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "version": 1,
                "providers": [{
                    "id": "plugin-provider",
                    "appId": "future-client",
                    "adapter": {
                        "pluginId": "example.plugin",
                        "pluginVersion": "2.0.0",
                        "adapterId": "example.adapter",
                        "contractMajor": 2,
                        "schemaVersion": 3,
                        "futureAdapterField": {"mode": "opaque"}
                    },
                    "name": "Future",
                    "settings": {"token": "secret"},
                    "futureProviderField": [1, 2, 3]
                }]
            }))
            .expect("serialize seed"),
        )
        .expect("seed plugin record");

        let store = ProviderStore::new(path.clone());
        store
            .create(draft("claude", "builtin.claude.api-key", "Work"))
            .expect("create builtin provider");
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("read provider file"))
                .expect("parse provider file");
        let plugin = &stored["providers"][0];

        assert_eq!(plugin["adapter"]["contractMajor"], 2);
        assert_eq!(
            plugin["adapter"]["futureAdapterField"],
            json!({"mode": "opaque"})
        );
        assert_eq!(plugin["futureProviderField"], json!([1, 2, 3]));
        assert_eq!(stored["providers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_plugin_record_cannot_be_edited_through_a_builtin_adapter() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        let plugin_provider = ProviderRecord {
            id: "plugin-provider".to_owned(),
            app_id: "claude".to_owned(),
            adapter: crate::provider::AdapterReference {
                plugin_id: "example.plugin".to_owned(),
                plugin_version: "1.0.0".to_owned(),
                adapter_id: "builtin.claude.api-key".to_owned(),
                contract_major: 1,
                schema_version: 1,
                extensions: Map::new(),
            },
            name: "Plugin".to_owned(),
            settings: settings(json!({"token": "secret"})),
            extensions: Map::new(),
        };
        ProviderStore::new(path.clone())
            .write(&ProviderFile {
                version: STORE_VERSION,
                providers: vec![plugin_provider.clone()],
            })
            .expect("seed plugin record");

        let store = ProviderStore::new(path);
        let result = store.update(
            &plugin_provider.id,
            ProviderUpdate {
                name: "Changed".to_owned(),
                settings: settings(json!({"apiKey": "stolen"})),
            },
        );

        assert!(matches!(result, Err(StoreError::InvalidProvider(_))));
        assert_eq!(store.read().unwrap().providers, [plugin_provider]);
    }

    #[test]
    fn a_missing_adapter_version_is_read_only() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        let store = ProviderStore::new(path);
        let mut provider = store
            .create(draft("claude", "builtin.claude.api-key", "Work"))
            .expect("create provider");
        provider.adapter.plugin_version = "0.0.9".to_owned();
        store
            .write(&ProviderFile {
                version: STORE_VERSION,
                providers: vec![provider.clone()],
            })
            .expect("seed old adapter version");

        let result = store.update(
            &provider.id,
            ProviderUpdate {
                name: "Changed".to_owned(),
                settings: settings(json!({"apiKey": "new-secret"})),
            },
        );

        assert!(matches!(result, Err(StoreError::InvalidProvider(_))));
        assert_eq!(store.read().unwrap().providers, [provider]);
    }

    #[test]
    fn independent_stores_serialize_mutations_with_the_sidecar_lock() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        let first = ProviderStore::new(path.clone());
        let held_lock = first.lock_file().expect("hold first store lock");
        let second = ProviderStore::new(path);
        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            started_tx.send(()).expect("signal worker");
            let result = second.create(draft("claude", "builtin.claude.api-key", "Second process"));
            result_tx.send(result).expect("send mutation result");
        });
        started_rx.recv().expect("worker started");
        assert!(result_rx.recv_timeout(Duration::from_millis(100)).is_err());

        drop(held_lock);
        result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second store unblocked")
            .expect("second store mutation");
        worker.join().expect("join worker");
        first
            .create(draft("claude", "builtin.claude.api-key", "First process"))
            .expect("first store mutation");

        assert_eq!(first.list("claude").expect("list providers").len(), 2);
    }
}
