use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cc_switch_core::AppType;
use hmac::{Hmac, Mac};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
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
const MAX_NATIVE_SETTINGS_BYTES: usize = 1024 * 1024;

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
        Self::open(home.join(".cc-switch").join("cc-switch.db"))
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
        let connection = self.connect()?;
        Ok(self
            .stored_providers(&connection, Some(&app))?
            .into_iter()
            .map(|provider| provider.record)
            .collect())
    }

    pub fn with_providers<T, E>(
        &self,
        app_id: &str,
        action: impl FnOnce(&[ProviderRecord]) -> Result<T, E>,
    ) -> Result<Result<T, E>, StoreError> {
        let app = parse_app(app_id)?;
        let connection = self.connect()?;
        let providers = self
            .stored_providers(&connection, Some(&app))?
            .into_iter()
            .map(|provider| provider.record)
            .collect::<Vec<_>>();
        drop(connection);
        Ok(action(&providers))
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
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        let connection = self.connect()?;
        let providers = self
            .stored_providers(&connection, None)?
            .into_iter()
            .map(|provider| provider.record)
            .collect::<Vec<_>>();
        drop(connection);
        Ok(action(&providers))
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
        let (name, settings) = validate_draft(&app, &draft, &descriptor)?;
        self.insert_provider(&app, name, settings, &descriptor.reference)
            .map(Ok)
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
        let connection = self.connect()?;
        Ok(self
            .stored_providers(&connection, Some(&app))?
            .into_iter()
            .filter(|provider| provider.is_current)
            .map(|provider| CurrentProvider::from(&provider.record))
            .collect())
    }

    #[cfg(test)]
    pub fn set_current(
        &self,
        app_id: &str,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), StoreError> {
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
        transaction.commit()?;
        Ok(())
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
            "SELECT p.id, p.app_type, p.name, p.settings_config, p.is_current,
                    b.adapter_json
             FROM providers p
             LEFT JOIN {ADAPTER_BINDINGS_TABLE} b
               ON b.provider_id = p.id
              AND b.app_type = p.app_type
              AND b.provider_created_at = p.created_at
             WHERE (?1 IS NULL OR p.app_type = ?1)
             ORDER BY COALESCE(p.sort_index, 999999), p.created_at ASC, p.id ASC"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([app.map(AppType::as_str)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut providers = Vec::new();
        for row in rows {
            providers.push(self.stored_from_columns(row?)?);
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
            "SELECT p.id, p.app_type, p.name, p.settings_config, p.is_current,
                    b.adapter_json
             FROM providers p
             LEFT JOIN {ADAPTER_BINDINGS_TABLE} b
               ON b.provider_id = p.id
              AND b.app_type = p.app_type
              AND b.provider_created_at = p.created_at
             WHERE p.id = ?1 AND (?2 IS NULL OR p.app_type = ?2)
             LIMIT 1"
        );
        let columns = transaction
            .query_row(&sql, params![id, app.map(AppType::as_str)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .optional()?;
        columns
            .map(|columns| self.stored_from_columns(columns))
            .transpose()
    }

    fn stored_from_columns(
        &self,
        (id, app_id, name, raw_settings, is_current, raw_adapter): (
            String,
            String,
            String,
            String,
            bool,
            Option<String>,
        ),
    ) -> Result<StoredProvider, StoreError> {
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
        let revision =
            snapshot_revision(&id, &app, &name, &settings, &adapter, &self.revision_key)?;
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

fn validate_draft(
    app: &AppType,
    draft: &ProviderDraft,
    descriptor: &AdapterDescriptor,
) -> Result<(String, Value), StoreError> {
    if draft.app_id != app.as_str()
        || descriptor.app_id != app.as_str()
        || draft.adapter != descriptor.reference
    {
        return Err(StoreError::InvalidProvider(
            "the provider targets a different application or adapter".to_owned(),
        ));
    }
    if descriptor.reference.plugin_id == BUILTIN_PLUGIN_ID
        && descriptor.reference != native_adapter_reference(app)
    {
        return Err(StoreError::InvalidProvider(
            "the legacy Lite adapter cannot write CC Switch native provider data".to_owned(),
        ));
    }
    validate_settings(descriptor, &draft.settings).map_err(StoreError::InvalidProvider)?;
    let name = validate_name(&draft.name).map_err(StoreError::InvalidProvider)?;
    Ok((name, Value::Object(draft.settings.clone())))
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
    id: &str,
    app: &AppType,
    name: &str,
    settings: &Value,
    adapter: &AdapterReference,
    key: &[u8],
) -> Result<u64, StoreError> {
    let mut hasher =
        Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts revision keys of any length");
    for value in [id, app.as_str(), name] {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    for value in [serde_json::to_vec(settings), serde_json::to_vec(adapter)] {
        let value = value.map_err(|error| StoreError::InvalidStore(error.to_string()))?;
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(&value);
    }
    let digest = hasher.finalize().into_bytes();
    let mut first = [0_u8; 8];
    first.copy_from_slice(&digest[..8]);
    Ok((u64::from_le_bytes(first) & SAFE_JS_INTEGER_MASK).max(1))
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
        Connection::open(path)
            .unwrap()
            .execute(
                "UPDATE providers SET settings_config='{\"env\":\"changed-by-full\"}'
                 WHERE id=?1 AND app_type='codex'",
                [&created.id],
            )
            .unwrap();

        let result = store.delete("codex", &created.id, created.revision);
        assert!(matches!(result, Err(StoreError::Conflict(_))));
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
