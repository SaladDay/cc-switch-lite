use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cc_switch_core::AppType;
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
    AdapterReference, CurrentProvider, ProviderDraft, ProviderRecord, ProviderUpdate,
    BUILTIN_PLUGIN_ID,
};

const SAFE_JS_INTEGER_MASK: u64 = (1_u64 << 53) - 1;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const ADAPTER_BINDINGS_TABLE: &str = "cc_switch_lite_provider_adapters";
const MIGRATIONS_TABLE: &str = "cc_switch_lite_migrations";
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
    _extensions: Map<String, Value>,
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
        }
    }
}

#[derive(Clone)]
struct StoredProvider {
    record: ProviderRecord,
    is_current: bool,
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

    pub fn with_provider<T, E>(
        &self,
        app_id: &str,
        id: &str,
        expected_revision: u64,
        action: impl FnOnce(&ProviderRecord) -> Result<T, E>,
    ) -> Result<Result<T, E>, StoreError> {
        let app = parse_app(app_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let provider = self
            .stored_provider(&transaction, &app, id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        ensure_revision(&provider.record, expected_revision)?;
        let result = action(&provider.record);
        transaction.commit()?;
        Ok(result)
    }

    pub fn with_all_providers<T, E>(
        &self,
        action: impl FnOnce(&[ProviderRecord]) -> Result<T, E>,
    ) -> Result<Result<T, E>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let providers = self
            .stored_providers(&transaction, None)?
            .into_iter()
            .map(|provider| provider.record)
            .collect::<Vec<_>>();
        transaction.commit()?;
        Ok(action(&providers))
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
        for (offset, provider) in file.providers.iter().enumerate() {
            let app = parse_app(&provider.app_id)?;
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM providers WHERE id = ?1 AND app_type = ?2",
                    params![provider.id, app.as_str()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if exists {
                return Err(StoreError::InvalidStore(format!(
                    "legacy provider '{}' conflicts with an existing shared provider",
                    provider.id
                )));
            }

            let (settings, adapter) = migrate_legacy_provider(provider, &app)?;
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
                    validate_name(&provider.name).map_err(StoreError::InvalidProvider)?,
                    serde_json::to_string(&settings)
                        .map_err(|error| StoreError::InvalidStore(error.to_string()))?,
                    created_at,
                    sort_index,
                ],
            )?;
            self.save_adapter_binding(&transaction, &provider.id, &app, created_at, &adapter)?;
        }
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
        provider_factory: impl FnOnce() -> Result<(ProviderDraft, AdapterDescriptor), E>,
    ) -> Result<Result<ProviderRecord, E>, StoreError> {
        let app = parse_app(app_id)?;
        let (draft, descriptor) = match provider_factory() {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        let (name, settings, adapter) = validate_draft(&app, &draft, &descriptor)?;
        self.insert_provider(&app, name, settings, &adapter).map(Ok)
    }

    pub fn create_native(&self, draft: ProviderDraft) -> Result<ProviderRecord, StoreError> {
        let app = parse_app(&draft.app_id)?;
        if draft.adapter != native_adapter_reference(&app) {
            return Err(StoreError::InvalidProvider(
                "the provider does not use its native application adapter".to_owned(),
            ));
        }
        let name = validate_name(&draft.name).map_err(StoreError::InvalidProvider)?;
        validate_native_settings(&draft.settings)?;
        self.insert_provider(
            &app,
            name,
            Value::Object(draft.settings),
            &native_adapter_reference(&app),
        )
    }

    fn insert_provider(
        &self,
        app: &AppType,
        name: String,
        settings: Value,
        adapter: &AdapterReference,
    ) -> Result<ProviderRecord, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = Uuid::new_v4().to_string();
        let created_at = now_millis()?;
        let sort_index: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sort_index) + 1, 0) FROM providers WHERE app_type = ?1",
            [app.as_str()],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO providers
             (id, app_type, name, settings_config, created_at, sort_index, meta, is_current, in_failover_queue)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '{}', 0, 0)",
            params![
                id,
                app.as_str(),
                name,
                serde_json::to_string(&settings)
                    .map_err(|error| StoreError::InvalidProvider(error.to_string()))?,
                created_at,
                sort_index,
            ],
        )?;
        self.save_adapter_binding(&transaction, &id, app, created_at, adapter)?;
        let created = self
            .stored_provider(&transaction, app, &id)?
            .ok_or_else(|| StoreError::NotFound(id.clone()))?
            .record;
        transaction.commit()?;
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
        if current.record.adapter == native_adapter_reference(&app) {
            validate_native_settings(&update.settings)?;
        } else {
            let descriptor = match descriptor_factory(&current.record, &update) {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
            if descriptor.app_id != current.record.app_id
                || descriptor.reference != current.record.adapter
            {
                return Err(StoreError::InvalidProvider(
                    "the resolved adapter does not own this provider".to_owned(),
                ));
            }
            validate_settings(&descriptor, &update.settings)
                .map_err(StoreError::InvalidProvider)?;
        }
        let name = validate_name(&update.name).map_err(StoreError::InvalidProvider)?;
        let settings = Value::Object(update.settings);
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
        let created = if self.path.exists() {
            false
        } else {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&self.path) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(source) => {
                    return Err(StoreError::Io {
                        path: self.path.clone(),
                        source,
                    });
                }
            }
        };
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
                PRIMARY KEY (provider_id, app_type)
            );
            CREATE TABLE IF NOT EXISTS {MIGRATIONS_TABLE} (
                id TEXT PRIMARY KEY,
                completed_at INTEGER NOT NULL
            );"
        ))?;
        self.verify_provider_schema(&connection)?;
        drop(connection);

        #[cfg(unix)]
        if created {
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
            "SELECT p.*, b.adapter_json AS lite_adapter_json
             FROM providers p
             LEFT JOIN {ADAPTER_BINDINGS_TABLE} b
               ON b.provider_id = p.id
              AND b.app_type = p.app_type
              AND b.provider_created_at = p.created_at
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
            "SELECT p.*, b.adapter_json AS lite_adapter_json
             FROM providers p
             LEFT JOIN {ADAPTER_BINDINGS_TABLE} b
               ON b.provider_id = p.id
              AND b.app_type = p.app_type
              AND b.provider_created_at = p.created_at
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
        let raw_settings: String = row.get("settings_config")?;
        let is_current: bool = row.get("is_current")?;
        let raw_adapter: Option<String> = row.get("lite_adapter_json")?;
        let app = parse_app(&app_id)?;
        let settings: Value = serde_json::from_str(&raw_settings).map_err(|_| {
            StoreError::InvalidStore(format!("provider '{id}' has invalid settings"))
        })?;
        let settings_object = settings.as_object().cloned().ok_or_else(|| {
            StoreError::InvalidStore(format!("provider '{id}' settings must be an object"))
        })?;
        let adapter = match raw_adapter {
            Some(raw) => serde_json::from_str::<AdapterReference>(&raw).map_err(|_| {
                StoreError::InvalidStore(format!("provider '{id}' has an invalid adapter binding"))
            })?,
            None => native_adapter_reference(&app),
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
                extensions: Map::new(),
            },
            is_current,
        })
    }

    fn save_adapter_binding(
        &self,
        transaction: &Transaction<'_>,
        id: &str,
        app: &AppType,
        created_at: i64,
        adapter: &AdapterReference,
    ) -> Result<(), StoreError> {
        if adapter.plugin_id == BUILTIN_PLUGIN_ID && *adapter == native_adapter_reference(app) {
            return Ok(());
        }
        let raw = serde_json::to_string(adapter)
            .map_err(|error| StoreError::InvalidProvider(error.to_string()))?;
        transaction.execute(
            &format!(
                "INSERT INTO {ADAPTER_BINDINGS_TABLE}
                 (provider_id, app_type, provider_created_at, adapter_json)
                 VALUES (?1, ?2, ?3, ?4)"
            ),
            params![id, app.as_str(), created_at, raw],
        )?;
        Ok(())
    }
}

fn parse_app(app_id: &str) -> Result<AppType, StoreError> {
    app_id.parse::<AppType>().map_err(|_| {
        StoreError::InvalidProvider(format!("application '{app_id}' is not supported"))
    })
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
    let mut identities = HashSet::new();
    for provider in &file.providers {
        if provider.id.is_empty()
            || provider.revision == 0
            || provider.adapter.plugin_id.is_empty()
            || provider.adapter.plugin_version.is_empty()
            || provider.adapter.adapter_id.is_empty()
            || provider.adapter.contract_major == 0
            || provider.adapter.schema_version == 0
            || !identities.insert((provider.app_id.as_str(), provider.id.as_str()))
        {
            return Err(StoreError::InvalidStore(
                "a legacy provider record is incomplete or duplicated".to_owned(),
            ));
        }
        parse_app(&provider.app_id)?;
        validate_name(&provider.name).map_err(StoreError::InvalidProvider)?;
    }
    Ok(())
}

fn migrate_legacy_provider(
    provider: &ProviderRecord,
    app: &AppType,
) -> Result<(Value, AdapterReference), StoreError> {
    let legacy = crate::provider::adapter_for_reference(&provider.app_id, &provider.adapter);
    if legacy.is_none() {
        let settings = Value::Object(provider.settings.clone());
        validate_native_settings(&provider.settings)?;
        return Ok((settings, provider.adapter.clone()));
    }

    let settings =
        legacy_settings_as_native(app, &provider.name, &provider.settings, &provider.id)?;
    Ok((settings, native_adapter_reference(app)))
}

fn legacy_settings_as_native(
    app: &AppType,
    name: &str,
    settings: &Map<String, Value>,
    identity: &str,
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
            let mut env = Map::new();
            env.insert(
                "ANTHROPIC_API_KEY".to_owned(),
                Value::String(api_key.to_owned()),
            );
            if let Some(base_url) = base_url {
                env.insert(
                    "ANTHROPIC_BASE_URL".to_owned(),
                    Value::String(base_url.to_owned()),
                );
            }
            if let Some(model) = model {
                env.insert(
                    "ANTHROPIC_MODEL".to_owned(),
                    Value::String(model.to_owned()),
                );
            }
            Value::Object(Map::from_iter([("env".to_owned(), Value::Object(env))]))
        }
        AppType::Codex => {
            let mut document = toml_edit::DocumentMut::new();
            document["model_provider"] = toml_edit::value("custom");
            if let Some(model) = model {
                document["model"] = toml_edit::value(model);
            }
            let mut route = toml_edit::Table::new();
            route["name"] = toml_edit::value(name);
            route["base_url"] = toml_edit::value(base_url.unwrap_or("https://api.openai.com/v1"));
            route["wire_api"] = toml_edit::value("responses");
            route["requires_openai_auth"] = toml_edit::value(true);
            let mut routes = toml_edit::Table::new();
            routes.insert("custom", toml_edit::Item::Table(route));
            document["model_providers"] = toml_edit::Item::Table(routes);

            Value::Object(Map::from_iter([
                (
                    "auth".to_owned(),
                    Value::Object(Map::from_iter([(
                        "OPENAI_API_KEY".to_owned(),
                        Value::String(api_key.to_owned()),
                    )])),
                ),
                ("config".to_owned(), Value::String(document.to_string())),
            ]))
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

fn validate_draft(
    app: &AppType,
    draft: &ProviderDraft,
    descriptor: &AdapterDescriptor,
) -> Result<(String, Value, AdapterReference), StoreError> {
    if draft.app_id != app.as_str()
        || descriptor.app_id != app.as_str()
        || draft.adapter != descriptor.reference
    {
        return Err(StoreError::InvalidProvider(
            "the provider targets a different application or adapter".to_owned(),
        ));
    }
    validate_settings(descriptor, &draft.settings).map_err(StoreError::InvalidProvider)?;
    let name = validate_name(&draft.name).map_err(StoreError::InvalidProvider)?;
    if descriptor.reference.plugin_id == BUILTIN_PLUGIN_ID
        && descriptor.reference != native_adapter_reference(app)
    {
        let settings = legacy_settings_as_native(app, &name, &draft.settings, "new provider")?;
        return Ok((name, settings, native_adapter_reference(app)));
    }
    Ok((
        name,
        Value::Object(draft.settings.clone()),
        descriptor.reference.clone(),
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
            .create_resolved_from("claude", || Ok::<_, StoreError>((draft, descriptor)))
            .unwrap()
            .unwrap();

        assert_eq!(created.adapter, native_adapter_reference(&AppType::Claude));
        assert_eq!(created.settings["env"]["ANTHROPIC_API_KEY"], "secret");
        assert_eq!(
            created.settings["env"]["ANTHROPIC_BASE_URL"],
            "https://example.com"
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
                            "model": "claude-model"
                        }
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
        let native = providers
            .iter()
            .find(|provider| provider.id == "legacy-claude")
            .unwrap();
        assert_eq!(native.adapter, native_adapter_reference(&AppType::Claude));
        assert_eq!(native.settings["env"]["ANTHROPIC_API_KEY"], "claude-secret");
        let plugin = providers
            .iter()
            .find(|provider| provider.id == "legacy-plugin")
            .unwrap();
        assert_eq!(plugin.adapter.plugin_id, "example.plugin");
        assert_eq!(plugin.adapter.extensions["future"], true);
        assert_eq!(plugin.settings["token"], "plugin-secret");
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
        let expected = descriptor.reference.clone();
        let mut plugin_draft = draft(&app, "Plugin", "secret");
        plugin_draft.adapter = expected.clone();
        plugin_draft.settings =
            Map::from_iter([("env".to_owned(), Value::String("secret".to_owned()))]);
        let created = store
            .create_resolved_from(app.as_str(), || {
                Ok::<_, StoreError>((plugin_draft, descriptor))
            })
            .unwrap()
            .unwrap();

        assert_eq!(created.adapter, expected);
        assert!(created.extensions.is_empty());
    }
}
