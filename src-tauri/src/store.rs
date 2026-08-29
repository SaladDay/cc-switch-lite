use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cc_switch_core::AppType;
use fs4::{FileExt, TryLockError};
use hmac::{Hmac, Mac};
use rusqlite::{
    params, types::ValueRef, Connection, OpenFlags, OptionalExtension, Row, Transaction,
    TransactionBehavior,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;

use crate::provider::{
    native_adapter_reference, validate_name, validate_settings, AdapterDescriptor,
    AdapterReference, CurrentProvider, NativeImport, ProviderDraft, ProviderRecord, ProviderUpdate,
    BUILTIN_PLUGIN_ID,
};

const SAFE_JS_INTEGER_MASK: u64 = (1_u64 << 53) - 1;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const ADAPTER_BINDINGS_TABLE: &str = "cc_switch_lite_provider_adapters";
const MIGRATIONS_TABLE: &str = "cc_switch_lite_migrations";
const MIGRATION_RECORDS_TABLE: &str = "cc_switch_lite_migration_records";
const STORE_EXTENSIONS_TABLE: &str = "cc_switch_lite_store_extensions";
const LEGACY_PROVIDER_MIGRATION: &str = "providers-json-v1";
const LEGACY_STORE_VERSION: u32 = 1;
const MAX_LEGACY_STORE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_NATIVE_SETTINGS_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyProviderFile {
    version: u32,
    providers: Vec<ProviderRecord>,
    #[serde(default, flatten)]
    extensions: Map<String, Value>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("shared provider database I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("shared provider database failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("shared provider data is invalid: {0}")]
    InvalidStore(String),
    #[error("provider is invalid: {0}")]
    InvalidProvider(String),
    #[error("provider '{0}' was not found")]
    NotFound(String),
    #[error("provider '{0}' changed; reload and try again")]
    Conflict(String),
    #[error("provider '{0}' is currently active")]
    CurrentProvider(String),
    #[error("shared provider update failed and live recovery was incomplete: {0}")]
    Recovery(String),
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } | Self::Database(_) => "storage_error",
            Self::InvalidStore(_) => "invalid_store",
            Self::InvalidProvider(_) => "invalid_provider",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::CurrentProvider(_) => "provider_in_use",
            Self::Recovery(_) => "recovery_failed",
        }
    }
}

#[derive(Clone)]
struct StoredProvider {
    record: ProviderRecord,
    is_current: bool,
    native_settings: Value,
}

struct ProviderBinding<'a> {
    adapter: &'a AdapterReference,
    extensions: &'a Map<String, Value>,
    compatibility_settings: Option<&'a Value>,
}

struct NewProvider {
    id: String,
    name: String,
    settings: Value,
    category: Option<String>,
    metadata: Value,
}

pub struct ProviderStore {
    path: PathBuf,
    revision_key: [u8; 16],
}

impl ProviderStore {
    pub fn from_home(home: &Path) -> Result<Self, StoreError> {
        Self::open(database_path(home))
    }

    pub fn open(path: PathBuf) -> Result<Self, StoreError> {
        let store = Self {
            path,
            revision_key: *Uuid::new_v4().as_bytes(),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn list(&self, app_id: &str) -> Result<Vec<ProviderRecord>, StoreError> {
        let app = parse_app(app_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let providers = self
            .stored_providers(&transaction, Some(&app))?
            .into_iter()
            .map(|provider| provider.record)
            .collect();
        transaction.commit()?;
        Ok(providers)
    }

    pub fn switch_with_provider<T, E>(
        &self,
        app_id: &str,
        id: &str,
        expected_revision: u64,
        action: impl FnOnce(&ProviderRecord, Option<&str>) -> Result<T, E>,
        rollback: impl FnOnce(T) -> Result<(), String>,
    ) -> Result<Result<(), E>, StoreError> {
        let app = parse_app(app_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let provider = self
            .stored_provider(&transaction, &app, id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        ensure_revision(&provider.record, expected_revision)?;
        let has_settings_table = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'settings'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        let common_snippet: Option<String> = if has_settings_table {
            transaction
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    [format!("common_config_{}", app.as_str())],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            None
        };
        let receipt = match action(&provider.record, common_snippet.as_deref()) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let finalize = (|| -> Result<(), StoreError> {
            if app.is_additive_mode() {
                let mut metadata =
                    provider
                        .record
                        .metadata
                        .as_object()
                        .cloned()
                        .ok_or_else(|| {
                            StoreError::InvalidStore(format!(
                                "provider '{id}' metadata is not an object"
                            ))
                        })?;
                metadata.insert("liveConfigManaged".to_owned(), Value::Bool(true));
                let changed = transaction.execute(
                    "UPDATE providers SET meta = ?1 WHERE id = ?2 AND app_type = ?3",
                    params![
                        serde_json::to_string(&Value::Object(metadata))
                            .map_err(|error| StoreError::InvalidProvider(error.to_string()))?,
                        id,
                        app.as_str(),
                    ],
                )?;
                if changed != 1 {
                    return Err(StoreError::Conflict(id.to_owned()));
                }
            } else {
                transaction.execute(
                    "UPDATE providers SET is_current = 0 WHERE app_type = ?1",
                    [app.as_str()],
                )?;
                let changed = transaction.execute(
                    "UPDATE providers SET is_current = 1 WHERE id = ?1 AND app_type = ?2",
                    params![id, app.as_str()],
                )?;
                if changed != 1 {
                    return Err(StoreError::Conflict(id.to_owned()));
                }
            }
            transaction.commit()?;
            Ok(())
        })();
        if let Err(error) = finalize {
            if let Err(rollback_error) = rollback(receipt) {
                return Err(StoreError::Recovery(format!(
                    "database error: {error}; live recovery error: {rollback_error}"
                )));
            }
            return Err(error);
        }
        Ok(Ok(()))
    }

    pub fn with_all_providers<T, E>(
        &self,
        action: impl FnOnce(&[ProviderRecord]) -> Result<T, E>,
    ) -> Result<Result<T, E>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let providers = self
            .stored_providers(&transaction, None)?
            .into_iter()
            .map(|provider| provider.record)
            .collect::<Vec<_>>();
        let result = action(&providers);
        if result.is_err() {
            transaction.rollback()?;
            return Ok(result);
        }
        transaction.commit()?;
        Ok(result)
    }

    pub fn migrate_legacy(&self, path: &Path) -> Result<(), StoreError> {
        let connection = self.connect()?;
        let already_migrated = connection
            .query_row(
                &format!("SELECT 1 FROM {MIGRATIONS_TABLE} WHERE id = ?1"),
                [LEGACY_PROVIDER_MIGRATION],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if already_migrated {
            return Ok(());
        }
        drop(connection);

        let _legacy_lock = lock_legacy_store(path)?;
        let Some(file) = read_legacy_file(path)? else {
            return Ok(());
        };
        validate_legacy_file(&file)?;

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let already_migrated = transaction
            .query_row(
                &format!("SELECT 1 FROM {MIGRATIONS_TABLE} WHERE id = ?1"),
                [LEGACY_PROVIDER_MIGRATION],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if already_migrated {
            transaction.commit()?;
            return Ok(());
        }

        let base_time = now_millis()?;
        let mut identities = HashSet::new();
        for (offset, provider) in file.providers.iter().enumerate() {
            let app = match parse_app(&provider.app_id) {
                Ok(app) => app,
                Err(_) => {
                    archive_legacy_provider(&transaction, offset, "unsupported_app", provider)?;
                    continue;
                }
            };
            let identity = (app.as_str().to_owned(), provider.id.clone());
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM providers WHERE id = ?1 AND app_type = ?2",
                    params![provider.id, app.as_str()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !identities.insert(identity) || exists {
                archive_legacy_provider(&transaction, offset, "identity_conflict", provider)?;
                continue;
            }

            let name = match validate_legacy_provider(provider) {
                Ok(name) => name,
                Err(_) => {
                    archive_legacy_provider(&transaction, offset, "invalid_record", provider)?;
                    continue;
                }
            };
            let (settings, adapter, compatibility_settings) =
                match migrate_legacy_provider(provider, &app) {
                    Ok(migrated) => migrated,
                    Err(_) => {
                        archive_legacy_provider(&transaction, offset, "invalid_record", provider)?;
                        continue;
                    }
                };
            let created_at = base_time
                .checked_add(i64::try_from(offset).map_err(|_| {
                    StoreError::InvalidStore(
                        "legacy provider count exceeds SQLite range".to_owned(),
                    )
                })?)
                .ok_or_else(|| {
                    StoreError::InvalidStore("legacy provider timestamp overflow".to_owned())
                })?;
            let sort_index: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(sort_index) + 1, 0) FROM providers WHERE app_type = ?1",
                [app.as_str()],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO providers
                 (id, app_type, name, settings_config, created_at, sort_index, meta,
                  is_current, in_failover_queue)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, '{}', 0, 0)",
                params![
                    provider.id,
                    app.as_str(),
                    name,
                    serde_json::to_string(&settings)
                        .map_err(|error| StoreError::InvalidStore(error.to_string()))?,
                    created_at,
                    sort_index,
                ],
            )?;
            self.save_adapter_binding(
                &transaction,
                &provider.id,
                &app,
                created_at,
                ProviderBinding {
                    adapter: &adapter,
                    extensions: &provider.extensions,
                    compatibility_settings: compatibility_settings.as_ref(),
                },
            )?;
        }
        transaction.execute(
            &format!(
                "INSERT INTO {STORE_EXTENSIONS_TABLE} (id, extensions_json)
                 VALUES (?1, ?2)"
            ),
            params![
                LEGACY_PROVIDER_MIGRATION,
                serde_json::to_string(&file.extensions)
                    .map_err(|error| StoreError::InvalidStore(error.to_string()))?,
            ],
        )?;
        transaction.execute(
            &format!("INSERT INTO {MIGRATIONS_TABLE} (id, completed_at) VALUES (?1, ?2)"),
            params![LEGACY_PROVIDER_MIGRATION, now_millis()?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn create_resolved_from<E>(
        &self,
        app_id: &str,
        make_current_if_empty: bool,
        provider_factory: impl FnOnce() -> Result<(ProviderDraft, AdapterDescriptor), E>,
    ) -> Result<Result<ProviderRecord, E>, StoreError> {
        let app = parse_app(app_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (draft, descriptor) = match provider_factory() {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        let (name, settings, adapter, compatibility_settings) =
            validate_draft(&app, &draft, &descriptor)?;
        let created = self.insert_provider(
            &transaction,
            &app,
            name,
            settings,
            ProviderBinding {
                adapter: &adapter,
                extensions: &Map::new(),
                compatibility_settings: compatibility_settings.as_ref(),
            },
            make_current_if_empty,
        )?;
        transaction.commit()?;
        Ok(Ok(created))
    }

    pub fn create_native(&self, draft: ProviderDraft) -> Result<ProviderRecord, StoreError> {
        let app = parse_app(&draft.app_id)?;
        if !draft.adapter.same_identity(&native_adapter_reference(&app)) {
            return Err(StoreError::InvalidProvider(
                "the provider does not use its native application adapter".to_owned(),
            ));
        }
        let name = validate_name(&draft.name).map_err(StoreError::InvalidProvider)?;
        validate_native_settings(&draft.settings)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created = self.insert_provider(
            &transaction,
            &app,
            name,
            Value::Object(draft.settings),
            ProviderBinding {
                adapter: &draft.adapter,
                extensions: &Map::new(),
                compatibility_settings: None,
            },
            false,
        )?;
        transaction.commit()?;
        Ok(created)
    }

    pub fn import_native_batch_from<E>(
        &self,
        app_id: &str,
        provider_factory: impl FnOnce() -> Result<Vec<NativeImport>, E>,
    ) -> Result<Result<Vec<ProviderRecord>, E>, StoreError> {
        let app = parse_app(app_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let drafts = match provider_factory() {
            Ok(drafts) => drafts,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let mut imported = Vec::new();
        let mut batch_ids = HashSet::new();

        for imported_provider in drafts {
            let NativeImport {
                native_id: id,
                draft,
                name_is_explicit,
                category,
                metadata,
            } = imported_provider;
            if draft.app_id != app.as_str()
                || !draft.adapter.same_identity(&native_adapter_reference(&app))
            {
                return Err(StoreError::InvalidProvider(
                    "the imported provider does not use its native application adapter".to_owned(),
                ));
            }
            let name = validate_name(&draft.name).map_err(StoreError::InvalidProvider)?;
            validate_native_settings(&draft.settings)?;
            if id.trim().is_empty() {
                return Err(StoreError::InvalidProvider(
                    "an imported native provider key cannot be empty".to_owned(),
                ));
            }
            let mut imported_metadata = metadata.as_object().cloned().ok_or_else(|| {
                StoreError::InvalidProvider("native provider metadata must be an object".to_owned())
            })?;
            if app.is_additive_mode() {
                imported_metadata.insert("liveConfigManaged".to_owned(), Value::Bool(true));
            }
            if !batch_ids.insert(id.clone()) {
                return Err(StoreError::InvalidProvider(
                    "the imported native provider keys are not unique".to_owned(),
                ));
            }
            if let Some(current) = self.stored_provider(&transaction, &app, &id)? {
                if !current
                    .record
                    .adapter
                    .same_identity(&native_adapter_reference(&app))
                {
                    return Err(StoreError::InvalidProvider(format!(
                        "provider '{id}' is owned by a different adapter"
                    )));
                }
                let mut merged_metadata = current
                    .record
                    .metadata
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                merged_metadata.extend(imported_metadata.clone());
                transaction.execute(
                    "UPDATE providers
                     SET name = CASE WHEN ?1 THEN ?2 ELSE name END,
                         settings_config = ?3, category = ?4, meta = ?5
                     WHERE id = ?6 AND app_type = ?7",
                    params![
                        name_is_explicit,
                        name,
                        serde_json::to_string(&Value::Object(draft.settings))
                            .map_err(|error| { StoreError::InvalidProvider(error.to_string()) })?,
                        category.or(current.record.category),
                        serde_json::to_string(&Value::Object(merged_metadata))
                            .map_err(|error| { StoreError::InvalidProvider(error.to_string()) })?,
                        id,
                        app.as_str(),
                    ],
                )?;
                imported.push(
                    self.stored_provider(&transaction, &app, &id)?
                        .ok_or_else(|| StoreError::NotFound(id.clone()))?
                        .record,
                );
                continue;
            }
            imported.push(self.insert_provider_with_id(
                &transaction,
                &app,
                NewProvider {
                    id,
                    name,
                    settings: Value::Object(draft.settings),
                    category,
                    metadata: Value::Object(imported_metadata),
                },
                ProviderBinding {
                    adapter: &draft.adapter,
                    extensions: &Map::new(),
                    compatibility_settings: None,
                },
                true,
            )?);
        }
        transaction.commit()?;
        Ok(Ok(imported))
    }

    fn insert_provider(
        &self,
        transaction: &Transaction<'_>,
        app: &AppType,
        name: String,
        settings: Value,
        binding: ProviderBinding<'_>,
        make_current_if_empty: bool,
    ) -> Result<ProviderRecord, StoreError> {
        let id = Uuid::new_v4().to_string();
        let mut metadata = Map::new();
        if app.is_additive_mode() {
            metadata.insert("liveConfigManaged".to_owned(), Value::Bool(false));
        }
        self.insert_provider_with_id(
            transaction,
            app,
            NewProvider {
                id,
                name,
                settings,
                category: None,
                metadata: Value::Object(metadata),
            },
            binding,
            make_current_if_empty,
        )
    }

    fn insert_provider_with_id(
        &self,
        transaction: &Transaction<'_>,
        app: &AppType,
        provider: NewProvider,
        binding: ProviderBinding<'_>,
        make_current_if_empty: bool,
    ) -> Result<ProviderRecord, StoreError> {
        let NewProvider {
            id,
            name,
            settings,
            category,
            metadata,
        } = provider;
        let created_at = now_millis()?;
        let sort_index: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sort_index) + 1, 0) FROM providers WHERE app_type = ?1",
            [app.as_str()],
            |row| row.get(0),
        )?;
        let make_current = if make_current_if_empty && !app.is_additive_mode() {
            transaction.query_row(
                "SELECT COUNT(*) = 0 FROM providers
                 WHERE app_type = ?1 AND is_current = 1",
                [app.as_str()],
                |row| row.get(0),
            )?
        } else {
            false
        };
        transaction.execute(
            "INSERT INTO providers
             (id, app_type, name, settings_config, category, created_at, sort_index, meta, is_current, in_failover_queue)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
            params![
                id,
                app.as_str(),
                name,
                serde_json::to_string(&settings)
                    .map_err(|error| StoreError::InvalidProvider(error.to_string()))?,
                category,
                created_at,
                sort_index,
                serde_json::to_string(&metadata)
                    .map_err(|error| StoreError::InvalidProvider(error.to_string()))?,
                make_current,
            ],
        )?;
        self.save_adapter_binding(transaction, &id, app, created_at, binding)?;
        let created = self
            .stored_provider(transaction, app, &id)?
            .ok_or_else(|| StoreError::NotFound(id.clone()))?
            .record;
        Ok(created)
    }

    pub fn update_from<E>(
        &self,
        app_id: &str,
        id: &str,
        update: ProviderUpdate,
        descriptor_factory: impl FnOnce(
            &ProviderRecord,
            &ProviderUpdate,
        ) -> Result<AdapterDescriptor, E>,
    ) -> Result<Result<ProviderRecord, E>, StoreError> {
        let app = parse_app(app_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = self
            .stored_provider(&transaction, &app, id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        ensure_revision(&current.record, update.expected_revision)?;
        if app == AppType::Hermes
            && current
                .record
                .settings
                .get("_cc_source")
                .and_then(Value::as_str)
                == Some("providers_dict")
        {
            return Err(StoreError::InvalidProvider(
                "Hermes dictionary providers are read-only in Lite".to_owned(),
            ));
        }
        let (settings, compatibility_settings) = if current
            .record
            .adapter
            .same_identity(&native_adapter_reference(&app))
        {
            validate_native_settings(&update.settings)?;
            (Value::Object(update.settings), None)
        } else {
            let descriptor = match descriptor_factory(&current.record, &update) {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
            if descriptor.app_id != current.record.app_id
                || !descriptor.reference.same_identity(&current.record.adapter)
            {
                return Err(StoreError::InvalidProvider(
                    "the resolved adapter does not own this provider".to_owned(),
                ));
            }
            let mut declared = Map::new();
            for field in &descriptor.fields {
                if let Some(value) = update.settings.get(&field.key) {
                    declared.insert(field.key.clone(), value.clone());
                }
            }
            validate_settings(&descriptor, &declared).map_err(StoreError::InvalidProvider)?;
            let mut merged = current.record.settings.clone();
            for field in &descriptor.fields {
                merged.remove(&field.key);
            }
            for (key, value) in declared {
                merged.insert(key, value);
            }
            if descriptor.reference.plugin_id == BUILTIN_PLUGIN_ID {
                (
                    apply_legacy_settings_to_native(
                        &app,
                        &update.name,
                        &merged,
                        id,
                        Some(current.native_settings),
                    )?,
                    Some(legacy_settings_extras(&merged)),
                )
            } else {
                (Value::Object(merged), None)
            }
        };
        let name = validate_name(&update.name).map_err(StoreError::InvalidProvider)?;
        transaction.execute(
            "UPDATE providers SET name = ?1, settings_config = ?2
             WHERE id = ?3 AND app_type = ?4",
            params![
                name,
                serde_json::to_string(&settings)
                    .map_err(|error| StoreError::InvalidProvider(error.to_string()))?,
                id,
                current.record.app_id,
            ],
        )?;
        if let Some(compatibility_settings) = compatibility_settings {
            transaction.execute(
                &format!(
                    "UPDATE {ADAPTER_BINDINGS_TABLE} SET compatibility_settings_json = ?1
                     WHERE provider_id = ?2 AND app_type = ?3"
                ),
                params![
                    serde_json::to_string(&compatibility_settings)
                        .map_err(|error| StoreError::InvalidProvider(error.to_string()))?,
                    id,
                    app.as_str(),
                ],
            )?;
        }
        let updated = self
            .stored_provider(&transaction, &app, id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?
            .record;
        transaction.commit()?;
        Ok(Ok(updated))
    }

    pub fn delete(&self, app_id: &str, id: &str, expected_revision: u64) -> Result<(), StoreError> {
        let app = parse_app(app_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = self
            .stored_provider(&transaction, &app, id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        ensure_revision(&current.record, expected_revision)?;
        if current.is_current && !app.is_additive_mode() {
            return Err(StoreError::CurrentProvider(id.to_owned()));
        }
        transaction.execute(
            &format!(
                "DELETE FROM {ADAPTER_BINDINGS_TABLE} WHERE provider_id = ?1 AND app_type = ?2"
            ),
            params![id, app.as_str()],
        )?;
        let changed = transaction.execute(
            "DELETE FROM providers WHERE id = ?1 AND app_type = ?2",
            params![id, app.as_str()],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(id.to_owned()));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_additive_with_provider<T, E>(
        &self,
        app_id: &str,
        id: &str,
        expected_revision: u64,
        action: impl FnOnce(&ProviderRecord) -> Result<T, E>,
        rollback: impl FnOnce(T) -> Result<(), String>,
    ) -> Result<Result<(), E>, StoreError> {
        let app = parse_app(app_id)?;
        if !app.is_additive_mode() {
            return Err(StoreError::InvalidProvider(format!(
                "application '{}' does not use additive provider configuration",
                app.as_str()
            )));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = self
            .stored_provider(&transaction, &app, id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        ensure_revision(&current.record, expected_revision)?;
        let receipt = match action(&current.record) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let finalize = (|| -> Result<(), StoreError> {
            transaction.execute(
                &format!(
                    "DELETE FROM {ADAPTER_BINDINGS_TABLE} WHERE provider_id = ?1 AND app_type = ?2"
                ),
                params![id, app.as_str()],
            )?;
            let changed = transaction.execute(
                "DELETE FROM providers WHERE id = ?1 AND app_type = ?2",
                params![id, app.as_str()],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict(id.to_owned()));
            }
            transaction.commit()?;
            Ok(())
        })();
        if let Err(error) = finalize {
            if let Err(rollback_error) = rollback(receipt) {
                return Err(StoreError::Recovery(format!(
                    "database error: {error}; live recovery error: {rollback_error}"
                )));
            }
            return Err(error);
        }
        Ok(Ok(()))
    }

    pub fn current(&self, app_id: &str) -> Result<Vec<CurrentProvider>, StoreError> {
        let app = parse_app(app_id)?;
        if app.is_additive_mode() {
            return Ok(Vec::new());
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let current = self
            .stored_providers(&transaction, Some(&app))?
            .into_iter()
            .filter(|provider| provider.is_current)
            .map(|provider| CurrentProvider::from(&provider.record))
            .collect::<Vec<_>>();
        if current.len() > 1 {
            return Err(StoreError::InvalidStore(format!(
                "application '{}' has more than one current provider",
                app.as_str()
            )));
        }
        transaction.commit()?;
        Ok(current)
    }

    #[cfg(test)]
    pub fn set_current(
        &self,
        app_id: &str,
        id: &str,
        expected_revision: u64,
    ) -> Result<ProviderRecord, StoreError> {
        let app = parse_app(app_id)?;
        if app.is_additive_mode() {
            return Err(StoreError::InvalidProvider(format!(
                "application '{}' uses additive provider configuration",
                app.as_str()
            )));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let provider = self
            .stored_provider(&transaction, &app, id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        ensure_revision(&provider.record, expected_revision)?;
        transaction.execute(
            "UPDATE providers SET is_current = 0 WHERE app_type = ?1",
            [app.as_str()],
        )?;
        let changed = transaction.execute(
            "UPDATE providers SET is_current = 1 WHERE id = ?1 AND app_type = ?2",
            params![id, app.as_str()],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict(id.to_owned()));
        }
        let current = self
            .stored_provider(&transaction, &app, id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?
            .record;
        transaction.commit()?;
        Ok(current)
    }

    fn initialize(&self) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        let exists = match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(StoreError::InvalidStore(
                    "shared provider database must not be a symbolic link".to_owned(),
                ));
            }
            Ok(metadata) if metadata.is_file() => true,
            Ok(_) => {
                return Err(StoreError::InvalidStore(
                    "shared provider database must be a regular file".to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(source) => {
                return Err(StoreError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        if !exists {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&self.path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(StoreError::Io {
                        path: self.path.clone(),
                        source,
                    });
                }
            }
        }
        let connection = self.connect()?;
        connection.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                website_url TEXT,
                category TEXT,
                created_at INTEGER,
                sort_index INTEGER,
                notes TEXT,
                icon TEXT,
                icon_color TEXT,
                meta TEXT NOT NULL DEFAULT '{{}}',
                is_current BOOLEAN NOT NULL DEFAULT 0,
                in_failover_queue BOOLEAN NOT NULL DEFAULT 0,
                PRIMARY KEY (id, app_type)
            );
            CREATE TABLE IF NOT EXISTS {ADAPTER_BINDINGS_TABLE} (
                provider_id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                provider_created_at INTEGER NOT NULL,
                adapter_json TEXT NOT NULL,
                compatibility_settings_json TEXT,
                record_extensions_json TEXT NOT NULL DEFAULT '{{}}',
                PRIMARY KEY (provider_id, app_type)
            );
            CREATE TABLE IF NOT EXISTS {MIGRATIONS_TABLE} (
                id TEXT PRIMARY KEY,
                completed_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS {MIGRATION_RECORDS_TABLE} (
                migration_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                reason TEXT NOT NULL,
                record_json TEXT NOT NULL,
                PRIMARY KEY (migration_id, position)
            );
            CREATE TABLE IF NOT EXISTS {STORE_EXTENSIONS_TABLE} (
                id TEXT PRIMARY KEY,
                extensions_json TEXT NOT NULL
            );
            CREATE TRIGGER IF NOT EXISTS cc_switch_lite_provider_adapter_delete
            AFTER DELETE ON providers
            BEGIN
                DELETE FROM {ADAPTER_BINDINGS_TABLE}
                WHERE provider_id = OLD.id AND app_type = OLD.app_type;
            END;"
        ))?;
        ensure_column(
            &connection,
            ADAPTER_BINDINGS_TABLE,
            "compatibility_settings_json",
            "TEXT",
        )?;
        ensure_column(
            &connection,
            ADAPTER_BINDINGS_TABLE,
            "record_extensions_json",
            "TEXT NOT NULL DEFAULT '{}'",
        )?;
        self.verify_provider_schema(&connection)?;
        drop(connection);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600)).map_err(
                |source| StoreError::Io {
                    path: self.path.clone(),
                    source,
                },
            )?;
        }
        Ok(())
    }

    fn verify_provider_schema(&self, connection: &Connection) -> Result<(), StoreError> {
        let mut statement = connection.prepare("PRAGMA table_info(providers)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<HashSet<_>, _>>()?;
        for required in [
            "id",
            "app_type",
            "name",
            "settings_config",
            "created_at",
            "sort_index",
            "meta",
            "is_current",
            "in_failover_queue",
        ] {
            if !columns.contains(required) {
                return Err(StoreError::InvalidStore(format!(
                    "providers table is missing required column '{required}'"
                )));
            }
        }
        Ok(())
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        Ok(connection)
    }

    fn stored_providers(
        &self,
        connection: &Connection,
        app: Option<&AppType>,
    ) -> Result<Vec<StoredProvider>, StoreError> {
        let sql = format!(
            "SELECT p.*, b.adapter_json AS lite_adapter_json,
                    b.compatibility_settings_json AS lite_compatibility_settings_json,
                    b.record_extensions_json AS lite_record_extensions_json
             FROM providers p
             LEFT JOIN {ADAPTER_BINDINGS_TABLE} b
               ON b.provider_id = p.id
              AND b.app_type = p.app_type
             WHERE (?1 IS NULL OR p.app_type = ?1)
             ORDER BY COALESCE(p.sort_index, 999999), p.created_at ASC, p.id ASC"
        );
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query([app.map(AppType::as_str)])?;
        let mut providers = Vec::new();
        while let Some(row) = rows.next()? {
            providers.push(self.stored_from_row(connection, row)?);
        }
        Ok(providers)
    }

    fn stored_provider(
        &self,
        transaction: &Transaction<'_>,
        app: &AppType,
        id: &str,
    ) -> Result<Option<StoredProvider>, StoreError> {
        self.stored_provider_query(transaction, Some(app), id)
    }

    fn stored_provider_query(
        &self,
        transaction: &Transaction<'_>,
        app: Option<&AppType>,
        id: &str,
    ) -> Result<Option<StoredProvider>, StoreError> {
        let sql = format!(
            "SELECT p.*, b.adapter_json AS lite_adapter_json,
                    b.compatibility_settings_json AS lite_compatibility_settings_json,
                    b.record_extensions_json AS lite_record_extensions_json
             FROM providers p
             LEFT JOIN {ADAPTER_BINDINGS_TABLE} b
               ON b.provider_id = p.id
              AND b.app_type = p.app_type
             WHERE p.id = ?1 AND (?2 IS NULL OR p.app_type = ?2)
             LIMIT 1"
        );
        let mut statement = transaction.prepare(&sql)?;
        let mut rows = statement.query(params![id, app.map(AppType::as_str)])?;
        rows.next()?
            .map(|row| self.stored_from_row(transaction, row))
            .transpose()
    }

    fn stored_from_row(
        &self,
        connection: &Connection,
        row: &Row<'_>,
    ) -> Result<StoredProvider, StoreError> {
        let id: String = row.get("id")?;
        let app_id: String = row.get("app_type")?;
        let name: String = row.get("name")?;
        let category: Option<String> = row.get("category")?;
        let raw_metadata: String = row.get("meta")?;
        let raw_settings: String = row.get("settings_config")?;
        let is_current: bool = row.get("is_current")?;
        let raw_adapter: Option<String> = row.get("lite_adapter_json")?;
        let raw_compatibility_settings: Option<String> =
            row.get("lite_compatibility_settings_json")?;
        let raw_extensions: Option<String> = row.get("lite_record_extensions_json")?;
        let app = parse_app(&app_id)?;
        let native_settings: Value = serde_json::from_str(&raw_settings).map_err(|_| {
            StoreError::InvalidStore(format!("provider '{id}' has invalid settings"))
        })?;
        let metadata: Value = serde_json::from_str(&raw_metadata).map_err(|_| {
            StoreError::InvalidStore(format!("provider '{id}' has invalid metadata"))
        })?;
        if !native_settings.is_object() {
            return Err(StoreError::InvalidStore(format!(
                "provider '{id}' settings must be an object"
            )));
        }
        let settings = match raw_compatibility_settings {
            Some(raw) => {
                let extras = serde_json::from_str(&raw).map_err(|_| {
                    StoreError::InvalidStore(format!(
                        "provider '{id}' has invalid compatibility settings"
                    ))
                })?;
                legacy_settings_from_native(&app, &native_settings, extras, &id)?
            }
            None => native_settings.clone(),
        };
        let settings_object = settings.as_object().cloned().ok_or_else(|| {
            StoreError::InvalidStore(format!("provider '{id}' settings must be an object"))
        })?;
        let adapter = match raw_adapter {
            Some(raw) => serde_json::from_str::<AdapterReference>(&raw).map_err(|_| {
                StoreError::InvalidStore(format!("provider '{id}' has an invalid adapter binding"))
            })?,
            None => native_adapter_reference(&app),
        };
        let extensions = match raw_extensions {
            Some(raw) => serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .ok_or_else(|| {
                    StoreError::InvalidStore(format!("provider '{id}' has invalid Lite extensions"))
                })?,
            None => Map::new(),
        };
        let revision = snapshot_revision(connection, row, &id, &app, &self.revision_key)?;
        Ok(StoredProvider {
            record: ProviderRecord {
                id,
                revision,
                app_id,
                adapter,
                name,
                settings: settings_object,
                category,
                metadata,
                extensions,
            },
            is_current,
            native_settings,
        })
    }

    fn save_adapter_binding(
        &self,
        transaction: &Transaction<'_>,
        id: &str,
        app: &AppType,
        created_at: i64,
        binding: ProviderBinding<'_>,
    ) -> Result<(), StoreError> {
        if binding.adapter.plugin_id == BUILTIN_PLUGIN_ID
            && binding
                .adapter
                .same_identity(&native_adapter_reference(app))
            && binding.adapter.extensions.is_empty()
            && binding.extensions.is_empty()
            && binding.compatibility_settings.is_none()
        {
            return Ok(());
        }
        let raw = serde_json::to_string(binding.adapter)
            .map_err(|error| StoreError::InvalidProvider(error.to_string()))?;
        let raw_extensions = serde_json::to_string(binding.extensions)
            .map_err(|error| StoreError::InvalidProvider(error.to_string()))?;
        let raw_compatibility_settings = binding
            .compatibility_settings
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| StoreError::InvalidProvider(error.to_string()))?;
        transaction.execute(
            &format!(
                "INSERT INTO {ADAPTER_BINDINGS_TABLE}
                 (provider_id, app_type, provider_created_at, adapter_json,
                  compatibility_settings_json, record_extensions_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            ),
            params![
                id,
                app.as_str(),
                created_at,
                raw,
                raw_compatibility_settings,
                raw_extensions,
            ],
        )?;
        Ok(())
    }
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    if !exists {
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

fn parse_app(app_id: &str) -> Result<AppType, StoreError> {
    app_id.parse::<AppType>().map_err(|_| {
        StoreError::InvalidProvider(format!("application '{app_id}' is not supported"))
    })
}

fn archive_legacy_provider(
    transaction: &Transaction<'_>,
    position: usize,
    reason: &str,
    provider: &ProviderRecord,
) -> Result<(), StoreError> {
    let position = i64::try_from(position).map_err(|_| {
        StoreError::InvalidStore("legacy provider count exceeds SQLite range".to_owned())
    })?;
    transaction.execute(
        &format!(
            "INSERT INTO {MIGRATION_RECORDS_TABLE}
             (migration_id, position, reason, record_json) VALUES (?1, ?2, ?3, ?4)"
        ),
        params![
            LEGACY_PROVIDER_MIGRATION,
            position,
            reason,
            serde_json::to_string(provider)
                .map_err(|error| StoreError::InvalidStore(error.to_string()))?,
        ],
    )?;
    Ok(())
}

fn lock_legacy_store(path: &Path) -> Result<File, StoreError> {
    let lock_path = path.with_extension("lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(&lock_path).map_err(|source| StoreError::Io {
        path: lock_path.clone(),
        source,
    })?;
    FileExt::try_lock(&lock).map_err(|error| match error {
        TryLockError::WouldBlock => StoreError::InvalidStore(
            "legacy provider store is in use; close the older Lite process and try again"
                .to_owned(),
        ),
        TryLockError::Error(source) => StoreError::Io {
            path: lock_path,
            source,
        },
    })?;
    Ok(lock)
}

fn database_path(home: &Path) -> PathBuf {
    let default = home.join(".cc-switch").join("cc-switch.db");

    #[cfg(windows)]
    if !default.exists() {
        if let Ok(legacy_home) = std::env::var("HOME") {
            let legacy_home = legacy_home.trim();
            let legacy = PathBuf::from(legacy_home)
                .join(".cc-switch")
                .join("cc-switch.db");
            if !legacy_home.is_empty() && legacy.exists() {
                return legacy;
            }
        }
    }

    default
}

fn read_legacy_file(path: &Path) -> Result<Option<LegacyProviderFile>, StoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(StoreError::InvalidStore(
                "legacy provider store must not be a symbolic link".to_owned(),
            ));
        }
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return Err(StoreError::InvalidStore(
                "legacy provider store must be a regular file".to_owned(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if metadata.len() > MAX_LEGACY_STORE_BYTES {
        return Err(StoreError::InvalidStore(format!(
            "legacy provider store exceeds the {MAX_LEGACY_STORE_BYTES} byte limit"
        )));
    }

    let file = File::open(path).map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })?;
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_LEGACY_STORE_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|source| StoreError::Io {
            path: path.to_owned(),
            source,
        })?;
    if contents.len() as u64 > MAX_LEGACY_STORE_BYTES {
        return Err(StoreError::InvalidStore(format!(
            "legacy provider store exceeds the {MAX_LEGACY_STORE_BYTES} byte limit"
        )));
    }
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(|error| StoreError::InvalidStore(format!("legacy provider store: {error}")))
}

fn validate_legacy_file(file: &LegacyProviderFile) -> Result<(), StoreError> {
    if file.version != LEGACY_STORE_VERSION {
        return Err(StoreError::InvalidStore(format!(
            "unsupported legacy provider store version {}",
            file.version
        )));
    }
    Ok(())
}

fn validate_legacy_provider(provider: &ProviderRecord) -> Result<String, StoreError> {
    if provider.id.is_empty()
        || provider.revision == 0
        || provider.adapter.plugin_id.is_empty()
        || provider.adapter.plugin_version.is_empty()
        || provider.adapter.adapter_id.is_empty()
        || provider.adapter.contract_major == 0
        || provider.adapter.schema_version == 0
    {
        return Err(StoreError::InvalidStore(
            "a legacy provider record is incomplete".to_owned(),
        ));
    }
    validate_name(&provider.name).map_err(StoreError::InvalidProvider)
}

fn migrate_legacy_provider(
    provider: &ProviderRecord,
    app: &AppType,
) -> Result<(Value, AdapterReference, Option<Value>), StoreError> {
    let legacy = crate::provider::adapter_for_reference(&provider.app_id, &provider.adapter);
    if legacy.is_none() {
        let settings = Value::Object(provider.settings.clone());
        validate_native_settings(&provider.settings)?;
        return Ok((settings, provider.adapter.clone(), None));
    }

    let settings = apply_legacy_settings_to_native(
        app,
        &provider.name,
        &provider.settings,
        &provider.id,
        None,
    )?;
    Ok((
        settings,
        provider.adapter.clone(),
        Some(legacy_settings_extras(&provider.settings)),
    ))
}

fn apply_legacy_settings_to_native(
    app: &AppType,
    name: &str,
    settings: &Map<String, Value>,
    identity: &str,
    native: Option<Value>,
) -> Result<Value, StoreError> {
    let api_key = settings
        .get("apiKey")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            StoreError::InvalidProvider(format!(
                "legacy provider '{identity}' is missing its API key"
            ))
        })?;
    let base_url = settings
        .get("baseUrl")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let model = settings
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());

    let settings = match app {
        AppType::Claude => {
            let mut root = native
                .unwrap_or_else(|| Value::Object(Map::new()))
                .as_object()
                .cloned()
                .ok_or_else(|| {
                    StoreError::InvalidProvider(format!(
                        "provider '{identity}' has invalid Claude settings"
                    ))
                })?;
            let env = root
                .entry("env".to_owned())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| {
                    StoreError::InvalidProvider(format!(
                        "provider '{identity}' has invalid Claude environment settings"
                    ))
                })?;
            env.insert(
                "ANTHROPIC_API_KEY".to_owned(),
                Value::String(api_key.to_owned()),
            );
            if let Some(base_url) = base_url {
                env.insert(
                    "ANTHROPIC_BASE_URL".to_owned(),
                    Value::String(base_url.to_owned()),
                );
            } else {
                env.remove("ANTHROPIC_BASE_URL");
            }
            if let Some(model) = model {
                env.insert(
                    "ANTHROPIC_MODEL".to_owned(),
                    Value::String(model.to_owned()),
                );
            } else {
                env.remove("ANTHROPIC_MODEL");
            }
            Value::Object(root)
        }
        AppType::Codex => {
            let mut root = native
                .unwrap_or_else(|| Value::Object(Map::new()))
                .as_object()
                .cloned()
                .ok_or_else(|| {
                    StoreError::InvalidProvider(format!(
                        "provider '{identity}' has invalid Codex settings"
                    ))
                })?;
            let auth = root
                .entry("auth".to_owned())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| {
                    StoreError::InvalidProvider(format!(
                        "provider '{identity}' has invalid Codex auth settings"
                    ))
                })?;
            auth.insert(
                "OPENAI_API_KEY".to_owned(),
                Value::String(api_key.to_owned()),
            );
            let existing_config = root
                .get("config")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut document = existing_config
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| {
                    StoreError::InvalidProvider(format!(
                        "provider '{identity}' has invalid Codex TOML"
                    ))
                })?;
            let route_id = document
                .get("model_provider")
                .and_then(toml_edit::Item::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("custom")
                .to_owned();
            document["model_provider"] = toml_edit::value(&route_id);
            if let Some(model) = model {
                document["model"] = toml_edit::value(model);
            } else {
                document.remove("model");
            }
            if document
                .get("model_providers")
                .is_some_and(|item| !item.is_table())
            {
                return Err(StoreError::InvalidProvider(format!(
                    "provider '{identity}' has invalid Codex provider routes"
                )));
            }
            if document.get("model_providers").is_none() {
                document["model_providers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let routes = document["model_providers"].as_table_mut().ok_or_else(|| {
                StoreError::InvalidProvider(format!(
                    "provider '{identity}' has invalid Codex provider routes"
                ))
            })?;
            if routes.get(&route_id).is_some_and(|item| !item.is_table()) {
                return Err(StoreError::InvalidProvider(format!(
                    "provider '{identity}' has an invalid active Codex route"
                )));
            }
            if routes.get(&route_id).is_none() {
                routes.insert(&route_id, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            let route = routes[&route_id].as_table_mut().ok_or_else(|| {
                StoreError::InvalidProvider(format!(
                    "provider '{identity}' has an invalid active Codex route"
                ))
            })?;
            route["name"] = toml_edit::value(name);
            route["base_url"] = toml_edit::value(base_url.unwrap_or("https://api.openai.com/v1"));
            route["wire_api"] = toml_edit::value("responses");
            route["requires_openai_auth"] = toml_edit::value(true);
            root.insert("config".to_owned(), Value::String(document.to_string()));
            Value::Object(root)
        }
        _ => {
            return Err(StoreError::InvalidProvider(format!(
                "legacy built-in adapter cannot target '{}'",
                app.as_str()
            )));
        }
    };
    Ok(settings)
}

fn legacy_settings_extras(settings: &Map<String, Value>) -> Value {
    let mut extras = settings.clone();
    for key in ["apiKey", "baseUrl", "model"] {
        extras.remove(key);
    }
    Value::Object(extras)
}

fn legacy_settings_from_native(
    app: &AppType,
    native: &Value,
    extras: Value,
    identity: &str,
) -> Result<Value, StoreError> {
    let native = native.as_object().ok_or_else(|| {
        StoreError::InvalidStore(format!("provider '{identity}' settings must be an object"))
    })?;
    let mut settings = extras.as_object().cloned().ok_or_else(|| {
        StoreError::InvalidStore(format!(
            "provider '{identity}' has invalid compatibility settings"
        ))
    })?;
    for key in ["apiKey", "baseUrl", "model"] {
        settings.remove(key);
    }
    match app {
        AppType::Claude => {
            let env = native.get("env").and_then(Value::as_object);
            copy_string_setting(env, "ANTHROPIC_API_KEY", &mut settings, "apiKey");
            copy_string_setting(env, "ANTHROPIC_BASE_URL", &mut settings, "baseUrl");
            copy_string_setting(env, "ANTHROPIC_MODEL", &mut settings, "model");
        }
        AppType::Codex => {
            copy_string_setting(
                native.get("auth").and_then(Value::as_object),
                "OPENAI_API_KEY",
                &mut settings,
                "apiKey",
            );
            if let Some(config) = native.get("config").and_then(Value::as_str) {
                let document = config.parse::<toml_edit::DocumentMut>().map_err(|_| {
                    StoreError::InvalidStore(format!(
                        "provider '{identity}' has invalid Codex TOML"
                    ))
                })?;
                if let Some(model) = document.get("model").and_then(toml_edit::Item::as_str) {
                    settings.insert("model".to_owned(), Value::String(model.to_owned()));
                }
                if let Some(route_id) = document
                    .get("model_provider")
                    .and_then(toml_edit::Item::as_str)
                {
                    if let Some(base_url) = document
                        .get("model_providers")
                        .and_then(toml_edit::Item::as_table)
                        .and_then(|routes| routes.get(route_id))
                        .and_then(toml_edit::Item::as_table)
                        .and_then(|route| route.get("base_url"))
                        .and_then(toml_edit::Item::as_str)
                    {
                        settings.insert("baseUrl".to_owned(), Value::String(base_url.to_owned()));
                    }
                }
            }
        }
        _ => {
            return Err(StoreError::InvalidStore(format!(
                "provider '{identity}' has an incompatible built-in adapter"
            )));
        }
    }
    Ok(Value::Object(settings))
}

fn copy_string_setting(
    source: Option<&Map<String, Value>>,
    source_key: &str,
    target: &mut Map<String, Value>,
    target_key: &str,
) {
    if let Some(value) = source
        .and_then(|source| source.get(source_key))
        .and_then(Value::as_str)
    {
        target.insert(target_key.to_owned(), Value::String(value.to_owned()));
    }
}

fn validate_draft(
    app: &AppType,
    draft: &ProviderDraft,
    descriptor: &AdapterDescriptor,
) -> Result<(String, Value, AdapterReference, Option<Value>), StoreError> {
    if draft.app_id != app.as_str()
        || descriptor.app_id != app.as_str()
        || !draft.adapter.same_identity(&descriptor.reference)
    {
        return Err(StoreError::InvalidProvider(
            "the provider targets a different application or adapter".to_owned(),
        ));
    }
    validate_settings(descriptor, &draft.settings).map_err(StoreError::InvalidProvider)?;
    let name = validate_name(&draft.name).map_err(StoreError::InvalidProvider)?;
    if descriptor.reference.plugin_id == BUILTIN_PLUGIN_ID
        && !descriptor
            .reference
            .same_identity(&native_adapter_reference(app))
    {
        let settings =
            apply_legacy_settings_to_native(app, &name, &draft.settings, "new provider", None)?;
        return Ok((
            name,
            settings,
            draft.adapter.clone(),
            Some(legacy_settings_extras(&draft.settings)),
        ));
    }
    Ok((
        name,
        Value::Object(draft.settings.clone()),
        draft.adapter.clone(),
        None,
    ))
}

fn validate_native_settings(settings: &Map<String, Value>) -> Result<(), StoreError> {
    let size = serde_json::to_vec(settings)
        .map_err(|error| StoreError::InvalidProvider(error.to_string()))?
        .len();
    if size > MAX_NATIVE_SETTINGS_BYTES {
        return Err(StoreError::InvalidProvider(format!(
            "native provider settings exceed the {MAX_NATIVE_SETTINGS_BYTES} byte limit"
        )));
    }
    Ok(())
}

fn ensure_revision(provider: &ProviderRecord, expected_revision: u64) -> Result<(), StoreError> {
    if provider.revision != expected_revision {
        return Err(StoreError::Conflict(provider.id.clone()));
    }
    Ok(())
}

fn now_millis() -> Result<i64, StoreError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| StoreError::InvalidStore(format!("system clock is invalid: {error}")))?
        .as_millis();
    i64::try_from(millis)
        .map_err(|_| StoreError::InvalidStore("system clock exceeds SQLite range".to_owned()))
}

fn snapshot_revision(
    connection: &Connection,
    provider_row: &Row<'_>,
    id: &str,
    app: &AppType,
    key: &[u8],
) -> Result<u64, StoreError> {
    let mut hasher =
        Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts revision keys of any length");
    hash_row(&mut hasher, provider_row)?;

    let endpoints_exist = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'provider_endpoints'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    hasher.update(&[u8::from(endpoints_exist)]);
    if endpoints_exist {
        let mut statement = connection.prepare(
            "SELECT * FROM provider_endpoints
             WHERE provider_id = ?1 AND app_type = ?2 ORDER BY id ASC",
        )?;
        let mut rows = statement.query(params![id, app.as_str()])?;
        while let Some(row) = rows.next()? {
            hash_row(&mut hasher, row)?;
        }
    }
    let digest = hasher.finalize().into_bytes();
    let mut first = [0_u8; 8];
    first.copy_from_slice(&digest[..8]);
    Ok((u64::from_le_bytes(first) & SAFE_JS_INTEGER_MASK).max(1))
}

fn hash_row(hasher: &mut Hmac<Sha256>, row: &Row<'_>) -> Result<(), rusqlite::Error> {
    let statement = row.as_ref();
    hasher.update(&(statement.column_count() as u64).to_le_bytes());
    for index in 0..statement.column_count() {
        let name = statement.column_name(index)?;
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        match row.get_ref(index)? {
            ValueRef::Null => hasher.update(&[0]),
            ValueRef::Integer(value) => {
                hasher.update(&[1]);
                hasher.update(&value.to_le_bytes());
            }
            ValueRef::Real(value) => {
                hasher.update(&[2]);
                hasher.update(&value.to_bits().to_le_bytes());
            }
            ValueRef::Text(value) => {
                hasher.update(&[3]);
                hasher.update(&(value.len() as u64).to_le_bytes());
                hasher.update(value);
            }
            ValueRef::Blob(value) => {
                hasher.update(&[4]);
                hasher.update(&(value.len() as u64).to_le_bytes());
                hasher.update(value);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_descriptor(app: &AppType) -> AdapterDescriptor {
        AdapterDescriptor {
            app_id: app.as_str().to_owned(),
            display_name: "Native test adapter".to_owned(),
            reference: native_adapter_reference(app),
            fields: vec![crate::provider::FormField {
                key: "env".to_owned(),
                label: "Environment".to_owned(),
                kind: crate::provider::FieldKind::Text,
                required: false,
                placeholder: String::new(),
                help: String::new(),
            }],
        }
    }

    fn draft(app: &AppType, name: &str, secret: &str) -> ProviderDraft {
        ProviderDraft {
            app_id: app.as_str().to_owned(),
            adapter: native_adapter_reference(app),
            name: name.to_owned(),
            settings: Map::from_iter([(
                "env".to_owned(),
                Value::Object(Map::from_iter([(
                    "API_KEY".to_owned(),
                    Value::String(secret.to_owned()),
                )])),
            )]),
        }
    }

    fn native_import(app: &AppType, id: &str, name: &str, secret: &str) -> NativeImport {
        NativeImport {
            native_id: id.to_owned(),
            draft: draft(app, name, secret),
            name_is_explicit: false,
            category: None,
            metadata: Value::Object(Map::new()),
        }
    }

    fn create_native(store: &ProviderStore, app: &AppType, name: &str) -> ProviderRecord {
        store.create_native(draft(app, name, "secret")).unwrap()
    }

    #[test]
    fn initializes_only_the_compatible_provider_schema_without_claiming_a_version() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join(".cc-switch/cc-switch.db");
        ProviderStore::open(path.clone()).expect("open provider store");
        let connection = Connection::open(&path).expect("inspect database");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read schema version");

        assert_eq!(version, 0);
        assert!(connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='providers'",
                [],
                |_| Ok(()),
            )
            .is_ok());

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
    fn every_core_application_uses_the_same_database_catalog() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ProviderStore::open(directory.path().join("cc-switch.db")).unwrap();

        for app in AppType::all() {
            let created = create_native(&store, &app, app.as_str());
            if app.is_additive_mode() {
                assert_eq!(created.metadata["liveConfigManaged"], false);
            }
            assert_eq!(store.list(app.as_str()).unwrap(), [created]);
        }
    }

    #[test]
    fn legacy_builtin_form_drafts_are_saved_in_the_native_schema() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ProviderStore::open(directory.path().join("cc-switch.db")).unwrap();
        let descriptor = crate::provider::built_in_adapters()
            .into_iter()
            .find(|adapter| adapter.app_id == "claude")
            .unwrap();
        let expected_adapter = descriptor.reference.clone();
        let draft = ProviderDraft {
            app_id: "claude".to_owned(),
            adapter: descriptor.reference.clone(),
            name: "Simple form".to_owned(),
            settings: Map::from_iter([
                ("apiKey".to_owned(), Value::String("secret".to_owned())),
                (
                    "baseUrl".to_owned(),
                    Value::String("https://example.com".to_owned()),
                ),
            ]),
        };

        let created = store
            .create_resolved_from("claude", false, || Ok::<_, StoreError>((draft, descriptor)))
            .unwrap()
            .unwrap();

        assert_eq!(created.adapter, expected_adapter);
        assert_eq!(created.settings["apiKey"], "secret");
        assert_eq!(created.settings["baseUrl"], "https://example.com");
        let stored: String = store
            .connect()
            .unwrap()
            .query_row(
                "SELECT settings_config FROM providers WHERE id=?1 AND app_type='claude'",
                [&created.id],
                |row| row.get(0),
            )
            .unwrap();
        let stored: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["env"]["ANTHROPIC_API_KEY"], "secret");
    }

    #[test]
    fn codex_compatibility_projection_keeps_native_extensions() {
        let native = serde_json::json!({
            "auth": {
                "OPENAI_API_KEY": "rotated-by-full",
                "futureAuth": {"keep": true}
            },
            "config": "model_provider = \"gateway\"\nmodel = \"old\"\nfuture_top = true\n\n[model_providers.gateway]\nname = \"Full\"\nbase_url = \"https://full.example/v1\"\nwire_api = \"responses\"\nfuture_route = \"keep\"\n",
            "futureRoot": {"keep": true}
        });
        let projected = legacy_settings_from_native(
            &AppType::Codex,
            &native,
            serde_json::json!({"futureSetting": {"keep": true}}),
            "codex-provider",
        )
        .unwrap();
        let mut projected = projected.as_object().unwrap().clone();
        assert_eq!(projected["apiKey"], "rotated-by-full");
        assert_eq!(projected["baseUrl"], "https://full.example/v1");
        assert_eq!(projected["futureSetting"]["keep"], true);
        projected.insert("apiKey".to_owned(), Value::String("new-key".to_owned()));
        projected.insert(
            "baseUrl".to_owned(),
            Value::String("https://lite.example/v1".to_owned()),
        );

        let merged = apply_legacy_settings_to_native(
            &AppType::Codex,
            "Renamed",
            &projected,
            "codex-provider",
            Some(native),
        )
        .unwrap();

        assert_eq!(merged["auth"]["OPENAI_API_KEY"], "new-key");
        assert_eq!(merged["auth"]["futureAuth"]["keep"], true);
        assert_eq!(merged["futureRoot"]["keep"], true);
        let config = merged["config"]
            .as_str()
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(config["future_top"].as_bool(), Some(true));
        assert_eq!(
            config["model_providers"]["gateway"]["base_url"].as_str(),
            Some("https://lite.example/v1")
        );
        assert_eq!(
            config["model_providers"]["gateway"]["future_route"].as_str(),
            Some("keep")
        );
    }

    #[test]
    fn native_crud_preserves_full_application_columns() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("cc-switch.db");
        let store = ProviderStore::open(path.clone()).unwrap();
        let created = create_native(&store, &AppType::Claude, "Work");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE providers SET website_url='https://example.com', category='custom',
                 notes='keep', icon='anthropic', icon_color='#123456', meta='{\"keep\":true}',
                 in_failover_queue=1 WHERE id=?1 AND app_type='claude'",
                [&created.id],
            )
            .unwrap();
        drop(connection);
        let refreshed = store.list("claude").unwrap().pop().unwrap();
        assert_eq!(refreshed.category.as_deref(), Some("custom"));
        assert_eq!(refreshed.metadata["keep"], true);
        let updated = store
            .update_from(
                "claude",
                &created.id,
                ProviderUpdate {
                    expected_revision: refreshed.revision,
                    name: "Primary".to_owned(),
                    settings: Map::from_iter([(
                        "env".to_owned(),
                        Value::Object(Map::from_iter([(
                            "API_KEY".to_owned(),
                            Value::String("new-secret".to_owned()),
                        )])),
                    )]),
                },
                |_, _| -> Result<AdapterDescriptor, StoreError> {
                    panic!("native updates do not resolve plugin descriptors")
                },
            )
            .unwrap()
            .unwrap();
        let connection = Connection::open(&path).unwrap();
        let untouched: (String, String, String, String, String, i64) = connection
            .query_row(
                "SELECT website_url, category, notes, icon, meta, in_failover_queue
                 FROM providers WHERE id=?1 AND app_type='claude'",
                [&created.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(updated.name, "Primary");
        assert_eq!(updated.settings["env"]["API_KEY"], "new-secret");
        assert_eq!(updated.category.as_deref(), Some("custom"));
        assert_eq!(updated.metadata["keep"], true);
        assert_eq!(
            untouched,
            (
                "https://example.com".to_owned(),
                "custom".to_owned(),
                "keep".to_owned(),
                "anthropic".to_owned(),
                "{\"keep\":true}".to_owned(),
                1,
            )
        );
    }

    #[test]
    fn hermes_dictionary_provider_cannot_be_changed_through_native_update() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("cc-switch.db");
        let store = ProviderStore::open(path.clone()).unwrap();
        let created = create_native(&store, &AppType::Hermes, "Managed");
        Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE providers SET settings_config = ?1
                 WHERE id = ?2 AND app_type = 'hermes'",
                params![
                    r#"{"_cc_source":"providers_dict","provider_key":"managed"}"#,
                    created.id,
                ],
            )
            .unwrap();
        let managed = store.list("hermes").unwrap().pop().unwrap();

        let result = store.update_from(
            "hermes",
            &managed.id,
            ProviderUpdate {
                expected_revision: managed.revision,
                name: "Writable".to_owned(),
                settings: Map::new(),
            },
            |_, _| -> Result<AdapterDescriptor, StoreError> {
                panic!("native updates do not resolve plugin descriptors")
            },
        );

        assert!(matches!(result, Err(StoreError::InvalidProvider(_))));
        let unchanged = store.list("hermes").unwrap().pop().unwrap();
        assert_eq!(unchanged.name, "Managed");
        assert_eq!(unchanged.settings["_cc_source"], "providers_dict");
    }

    #[test]
    fn native_batch_import_preserves_additive_keys_atomically() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ProviderStore::open(directory.path().join("cc-switch.db")).unwrap();
        let app = AppType::Pi;
        let imported = store
            .import_native_batch_from(app.as_str(), || {
                Ok::<_, StoreError>(vec![
                    native_import(&app, "anthropic", "Anthropic", "one"),
                    native_import(&app, "custom", "Custom", "two"),
                ])
            })
            .unwrap()
            .unwrap();

        assert_eq!(
            imported
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            ["anthropic", "custom"]
        );
        assert!(imported
            .iter()
            .all(|provider| provider.metadata["liveConfigManaged"] == true));

        let connection = store.connect().unwrap();
        connection
            .execute(
                "UPDATE providers SET category='custom', meta='{\"keep\":true}'
                 WHERE id='custom' AND app_type='pi'",
                [],
            )
            .unwrap();
        drop(connection);
        let refreshed = store
            .import_native_batch_from(app.as_str(), || {
                Ok::<_, StoreError>(vec![native_import(
                    &app,
                    "custom",
                    "Fallback changed",
                    "refreshed",
                )])
            })
            .unwrap()
            .unwrap();
        assert_eq!(store.list(app.as_str()).unwrap().len(), 2);
        assert_eq!(refreshed[0].settings["env"]["API_KEY"], "refreshed");
        assert_eq!(refreshed[0].name, "Custom");
        assert_eq!(refreshed[0].category.as_deref(), Some("custom"));
        assert_eq!(refreshed[0].metadata["keep"], true);
        assert_eq!(refreshed[0].metadata["liveConfigManaged"], true);

        let renamed = store
            .import_native_batch_from(app.as_str(), || {
                let mut imported = native_import(&app, "custom", "Explicit name", "again");
                imported.name_is_explicit = true;
                Ok::<_, StoreError>(vec![imported])
            })
            .unwrap()
            .unwrap();
        assert_eq!(renamed[0].name, "Explicit name");

        let duplicate = store.import_native_batch_from(app.as_str(), || {
            Ok::<_, StoreError>(vec![
                native_import(&app, "duplicate", "One", "one"),
                native_import(&app, "duplicate", "Two", "two"),
            ])
        });
        assert!(matches!(duplicate, Err(StoreError::InvalidProvider(_))));
        assert!(store
            .list(app.as_str())
            .unwrap()
            .iter()
            .all(|provider| provider.id != "duplicate"));
    }

    #[test]
    fn switch_reads_common_config_from_the_same_database_transaction() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ProviderStore::open(directory.path().join("cc-switch.db")).unwrap();
        let provider = create_native(&store, &AppType::Claude, "Claude");
        store
            .connect()
            .unwrap()
            .execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO settings (key, value)
                 VALUES ('common_config_claude', '{\"env\":{\"COMMON\":\"yes\"}}');",
            )
            .unwrap();

        store
            .switch_with_provider(
                "claude",
                &provider.id,
                provider.revision,
                |_, snippet| {
                    assert_eq!(snippet, Some(r#"{"env":{"COMMON":"yes"}}"#));
                    Ok::<(), &str>(())
                },
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();
    }

    #[test]
    fn additive_switch_marks_the_shared_row_as_live_managed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("cc-switch.db");
        let store = ProviderStore::open(path.clone()).unwrap();
        let created = create_native(&store, &AppType::OpenCode, "OpenCode");
        Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE providers SET meta = '{\"keep\":true,\"liveConfigManaged\":false}'
                 WHERE id = ?1 AND app_type = 'opencode'",
                [&created.id],
            )
            .unwrap();
        let provider = store.list("opencode").unwrap().pop().unwrap();

        store
            .switch_with_provider(
                "opencode",
                &provider.id,
                provider.revision,
                |_, _| Ok::<(), &str>(()),
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();

        let updated = store.list("opencode").unwrap().pop().unwrap();
        assert_eq!(updated.metadata["keep"], true);
        assert_eq!(updated.metadata["liveConfigManaged"], true);
    }

    #[test]
    fn external_full_app_edits_invalidate_optimistic_revisions() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("cc-switch.db");
        let store = ProviderStore::open(path.clone()).unwrap();
        let created = create_native(&store, &AppType::Codex, "Work");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE providers SET notes='changed-by-full'
                 WHERE id=?1 AND app_type='codex'",
                [&created.id],
            )
            .unwrap();

        let result = store.delete("codex", &created.id, created.revision);
        assert!(matches!(result, Err(StoreError::Conflict(_))));

        let refreshed = store.list("codex").unwrap().pop().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE provider_endpoints (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    provider_id TEXT NOT NULL,
                    app_type TEXT NOT NULL,
                    url TEXT NOT NULL,
                    added_at INTEGER,
                    FOREIGN KEY (provider_id, app_type)
                        REFERENCES providers(id, app_type) ON DELETE CASCADE
                );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO provider_endpoints (provider_id, app_type, url, added_at)
                 VALUES (?1, 'codex', 'https://example.com', 1)",
                [&created.id],
            )
            .unwrap();
        assert!(matches!(
            store.delete("codex", &created.id, refreshed.revision),
            Err(StoreError::Conflict(_))
        ));

        let refreshed = store.list("codex").unwrap().pop().unwrap();
        connection
            .execute(
                "ALTER TABLE providers ADD COLUMN future_full_field TEXT NOT NULL DEFAULT ''",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE providers SET future_full_field='changed'
                 WHERE id=?1 AND app_type='codex'",
                [&created.id],
            )
            .unwrap();
        assert!(matches!(
            store.delete("codex", &created.id, refreshed.revision),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn migrates_the_legacy_lite_store_once_without_losing_plugin_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("cc-switch.db");
        let legacy_path = directory.path().join("providers.json");
        let plugin_adapter = AdapterReference {
            plugin_id: "example.plugin".to_owned(),
            plugin_version: "1.2.3".to_owned(),
            adapter_id: "example.claude".to_owned(),
            contract_major: 1,
            schema_version: 2,
            extensions: Map::from_iter([("future".to_owned(), Value::Bool(true))]),
        };
        fs::write(
            &legacy_path,
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "futureFileState": {"keep": true},
                "providers": [
                    {
                        "id": "legacy-claude",
                        "revision": 4,
                        "appId": "claude",
                        "adapter": crate::provider::built_in_adapters()[0].reference,
                        "name": "Claude legacy",
                        "settings": {
                            "apiKey": "claude-secret",
                            "baseUrl": "https://claude.example",
                            "model": "claude-model",
                            "futureSetting": {"keep": true}
                        },
                        "futureProviderState": {"keep": true}
                    },
                    {
                        "id": "legacy-plugin",
                        "revision": 9,
                        "appId": "claude",
                        "adapter": plugin_adapter,
                        "name": "Plugin legacy",
                        "settings": {"token": "plugin-secret"}
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let store = ProviderStore::open(path.clone()).unwrap();

        store.migrate_legacy(&legacy_path).unwrap();
        store.migrate_legacy(&legacy_path).unwrap();
        let providers = store.list("claude").unwrap();

        assert_eq!(providers.len(), 2);
        let compatibility = providers
            .iter()
            .find(|provider| provider.id == "legacy-claude")
            .unwrap()
            .clone();
        assert!(crate::provider::adapter_for_reference("claude", &compatibility.adapter).is_some());
        assert_eq!(compatibility.settings["apiKey"], "claude-secret");
        assert_eq!(compatibility.settings["futureSetting"]["keep"], true);
        assert_eq!(
            compatibility.extensions["futureProviderState"]["keep"],
            true
        );
        let native: String = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT settings_config FROM providers
                 WHERE id='legacy-claude' AND app_type='claude'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let native: Value = serde_json::from_str(&native).unwrap();
        assert_eq!(native["env"]["ANTHROPIC_API_KEY"], "claude-secret");
        Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE providers SET settings_config=?1
                 WHERE id='legacy-claude' AND app_type='claude'",
                [serde_json::json!({
                    "env": {
                        "ANTHROPIC_API_KEY": "rotated-by-full",
                        "ANTHROPIC_BASE_URL": "https://full.example",
                        "futureEnv": {"keep": true}
                    },
                    "futureNative": {"keep": true}
                })
                .to_string()],
            )
            .unwrap();
        let refreshed = store
            .list("claude")
            .unwrap()
            .into_iter()
            .find(|provider| provider.id == "legacy-claude")
            .unwrap();
        assert_eq!(refreshed.settings["apiKey"], "rotated-by-full");
        assert_eq!(refreshed.settings["baseUrl"], "https://full.example");
        assert_eq!(refreshed.settings["futureSetting"]["keep"], true);
        let descriptor = crate::provider::built_in_adapters()
            .into_iter()
            .find(|adapter| adapter.app_id == "claude")
            .unwrap();
        store
            .update_from(
                "claude",
                "legacy-claude",
                ProviderUpdate {
                    expected_revision: refreshed.revision,
                    name: "Renamed".to_owned(),
                    settings: refreshed.settings,
                },
                |_, _| Ok::<_, StoreError>(descriptor),
            )
            .unwrap()
            .unwrap();
        let after_lite: String = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT settings_config FROM providers
                 WHERE id='legacy-claude' AND app_type='claude'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let after_lite: Value = serde_json::from_str(&after_lite).unwrap();
        assert_eq!(after_lite["env"]["ANTHROPIC_API_KEY"], "rotated-by-full");
        assert_eq!(after_lite["env"]["futureEnv"]["keep"], true);
        assert_eq!(after_lite["futureNative"]["keep"], true);
        let plugin = providers
            .iter()
            .find(|provider| provider.id == "legacy-plugin")
            .unwrap();
        assert_eq!(plugin.adapter.plugin_id, "example.plugin");
        assert_eq!(plugin.adapter.extensions["future"], true);
        assert_eq!(plugin.settings["token"], "plugin-secret");
        let root_extensions: String = Connection::open(&path)
            .unwrap()
            .query_row(
                &format!("SELECT extensions_json FROM {STORE_EXTENSIONS_TABLE} WHERE id=?1"),
                [LEGACY_PROVIDER_MIGRATION],
                |row| row.get(0),
            )
            .unwrap();
        let root_extensions: Value = serde_json::from_str(&root_extensions).unwrap();
        assert_eq!(root_extensions["futureFileState"]["keep"], true);
        assert!(legacy_path.exists());
        assert_eq!(
            Connection::open(path)
                .unwrap()
                .query_row(
                    &format!("SELECT COUNT(*) FROM {MIGRATIONS_TABLE}"),
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn legacy_migration_respects_the_previous_store_lock() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let legacy_path = directory.path().join("providers.json");
        fs::write(&legacy_path, br#"{"version":1,"providers":[]}"#).unwrap();
        let lock_path = legacy_path.with_extension("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        FileExt::lock(&lock).unwrap();
        let store = ProviderStore::open(directory.path().join("cc-switch.db")).unwrap();

        assert!(matches!(
            store.migrate_legacy(&legacy_path),
            Err(StoreError::InvalidStore(_))
        ));
        FileExt::unlock(&lock).unwrap();
        store.migrate_legacy(&legacy_path).unwrap();
    }

    #[test]
    fn legacy_migration_archives_conflicts_without_blocking_other_records() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let legacy_path = directory.path().join("providers.json");
        let store = ProviderStore::open(directory.path().join("cc-switch.db")).unwrap();
        store
            .connect()
            .unwrap()
            .execute(
                "INSERT INTO providers
                 (id, app_type, name, settings_config, created_at, sort_index, meta,
                  is_current, in_failover_queue)
                 VALUES ('same-id', 'claude', 'Existing', '{}', 1, 0, '{}', 0, 0)",
                [],
            )
            .unwrap();
        fs::write(
            &legacy_path,
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "providers": [
                    {
                        "id": "same-id",
                        "revision": 1,
                        "appId": "claude",
                        "adapter": native_adapter_reference(&AppType::Claude),
                        "name": "Conflicting",
                        "settings": {}
                    },
                    {
                        "id": "codex-kept",
                        "revision": 1,
                        "appId": "codex",
                        "adapter": native_adapter_reference(&AppType::Codex),
                        "name": "Codex kept",
                        "settings": {"auth": {}, "config": ""}
                    },
                    {
                        "id": "invalid-claude",
                        "revision": 1,
                        "appId": "claude",
                        "adapter": native_adapter_reference(&AppType::Claude),
                        "name": "",
                        "settings": {}
                    },
                    {
                        "id": "codex-kept",
                        "revision": 2,
                        "appId": "codex",
                        "adapter": native_adapter_reference(&AppType::Codex),
                        "name": "",
                        "settings": {"auth": {}, "config": ""}
                    },
                    {
                        "id": "future-kept",
                        "revision": 1,
                        "appId": "future-client",
                        "adapter": {
                            "pluginId": "example.future",
                            "pluginVersion": "1.0.0",
                            "adapterId": "future.adapter",
                            "contractMajor": 1,
                            "schemaVersion": 1
                        },
                        "name": "",
                        "settings": {"token": "secret"}
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        store.migrate_legacy(&legacy_path).unwrap();

        assert_eq!(store.list("codex").unwrap()[0].id, "codex-kept");
        let connection = store.connect().unwrap();
        let mut statement = connection
            .prepare(&format!(
                "SELECT reason, record_json FROM {MIGRATION_RECORDS_TABLE} ORDER BY position"
            ))
            .unwrap();
        let archived = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<(String, String)>, _>>()
            .unwrap();
        assert_eq!(archived.len(), 4);
        assert_eq!(archived[0].0, "identity_conflict");
        assert!(archived[0].1.contains("Conflicting"));
        assert_eq!(archived[1].0, "invalid_record");
        assert!(archived[1].1.contains("invalid-claude"));
        assert_eq!(archived[2].0, "identity_conflict");
        assert!(archived[2].1.contains("\"revision\":2"));
        assert_eq!(archived[3].0, "unsupported_app");
        assert!(archived[3].1.contains("future-kept"));
    }

    #[test]
    fn exclusive_current_rows_cannot_be_deleted_but_additive_rows_can() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ProviderStore::open(directory.path().join("cc-switch.db")).unwrap();
        let claude = create_native(&store, &AppType::Claude, "Claude");
        store
            .set_current("claude", &claude.id, claude.revision)
            .unwrap();
        let current = store.list("claude").unwrap().pop().unwrap();
        assert!(matches!(
            store.delete("claude", &current.id, current.revision),
            Err(StoreError::CurrentProvider(_))
        ));

        let alternate = create_native(&store, &AppType::Claude, "Alternate");
        store
            .switch_with_provider(
                "claude",
                &alternate.id,
                alternate.revision,
                |_, _| Err::<(), _>("live failed"),
                |_| Ok(()),
            )
            .unwrap()
            .unwrap_err();
        assert_eq!(store.current("claude").unwrap()[0].id, claude.id);
        let alternate = store
            .list("claude")
            .unwrap()
            .into_iter()
            .find(|provider| provider.id == alternate.id)
            .unwrap();
        store
            .connect()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_current_update
                 BEFORE UPDATE OF is_current ON providers
                 WHEN NEW.is_current = 1
                 BEGIN SELECT RAISE(ABORT, 'current rejected'); END;",
            )
            .unwrap();
        let live_applied = std::cell::Cell::new(false);
        assert!(store
            .switch_with_provider(
                "claude",
                &alternate.id,
                alternate.revision,
                |_, _| {
                    live_applied.set(true);
                    Ok::<(), &str>(())
                },
                |_| {
                    live_applied.set(false);
                    Ok(())
                },
            )
            .is_err());
        assert!(!live_applied.get());
        assert_eq!(store.current("claude").unwrap()[0].id, claude.id);
        store
            .connect()
            .unwrap()
            .execute("DROP TRIGGER reject_current_update", [])
            .unwrap();
        store
            .switch_with_provider(
                "claude",
                &alternate.id,
                alternate.revision,
                |_, _| Ok::<(), &str>(()),
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(store.current("claude").unwrap()[0].id, alternate.id);
        let codex = AppType::Codex;
        let descriptor = native_descriptor(&codex);
        let imported = store
            .create_resolved_from("codex", true, || {
                let mut imported = draft(&codex, "Imported", "secret");
                imported
                    .settings
                    .insert("env".to_owned(), Value::String("secret".to_owned()));
                Ok::<_, StoreError>((imported, descriptor))
            })
            .unwrap()
            .unwrap();
        assert_eq!(store.current("codex").unwrap()[0].id, imported.id);
        let later = store
            .create_resolved_from("codex", true, || {
                let mut later = draft(&codex, "Later", "later-secret");
                later
                    .settings
                    .insert("env".to_owned(), Value::String("later-secret".to_owned()));
                Ok::<_, StoreError>((later, native_descriptor(&codex)))
            })
            .unwrap()
            .unwrap();
        assert_ne!(later.id, imported.id);
        assert_eq!(store.current("codex").unwrap()[0].id, imported.id);

        let opencode = create_native(&store, &AppType::OpenCode, "OpenCode");
        store
            .delete("opencode", &opencode.id, opencode.revision)
            .unwrap();
    }

    #[test]
    fn plugin_bindings_are_internal_and_do_not_change_native_rows() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ProviderStore::open(directory.path().join("cc-switch.db")).unwrap();
        let app = AppType::Gemini;
        let mut descriptor = native_descriptor(&app);
        descriptor.reference.plugin_id = "example.plugin".to_owned();
        descriptor.reference.adapter_id = "example.gemini".to_owned();
        let descriptor_for_update = descriptor.clone();
        let mut expected = descriptor.reference.clone();
        expected
            .extensions
            .insert("futureAdapterState".to_owned(), Value::Bool(true));
        let mut plugin_draft = draft(&app, "Plugin", "secret");
        plugin_draft.adapter = expected.clone();
        plugin_draft.settings =
            Map::from_iter([("env".to_owned(), Value::String("secret".to_owned()))]);
        let created = store
            .create_resolved_from(app.as_str(), false, || {
                Ok::<_, StoreError>((plugin_draft, descriptor))
            })
            .unwrap()
            .unwrap();

        assert_eq!(created.adapter, expected);
        assert!(created.extensions.is_empty());
        store
            .connect()
            .unwrap()
            .execute(
                "UPDATE providers SET created_at = created_at + 1,
                 settings_config = '{\"env\":\"secret\",\"futureSetting\":{\"keep\":true}}'
                 WHERE id=?1 AND app_type=?2",
                params![created.id, app.as_str()],
            )
            .unwrap();
        let refreshed = store.list(app.as_str()).unwrap().pop().unwrap();
        assert_eq!(refreshed.adapter, expected);
        let updated = store
            .update_from(
                app.as_str(),
                &created.id,
                ProviderUpdate {
                    expected_revision: refreshed.revision,
                    name: "Updated".to_owned(),
                    settings: Map::from_iter([
                        ("env".to_owned(), Value::String("new-secret".to_owned())),
                        ("futureSetting".to_owned(), Value::Bool(false)),
                    ]),
                },
                |_, _| Ok::<_, StoreError>(descriptor_for_update),
            )
            .unwrap()
            .unwrap();
        assert_eq!(updated.settings["env"], "new-secret");
        assert_eq!(updated.settings["futureSetting"]["keep"], true);
    }
}
