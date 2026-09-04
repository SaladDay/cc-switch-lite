use std::{collections::BTreeMap, fmt, path::PathBuf};

use cc_switch_core::{
    builtin_app_registry, mcp_servers_equivalent, validate_mcp_server, validate_mcp_server_for_app,
    AppType, McpCatalogColumn, McpNativeSnapshot,
};
use cc_switch_store::{
    begin_immediate_transaction, delete_mcp_native_links, delete_mcp_server,
    ensure_mcp_native_link_schema, insert_mcp_server_catalog, read_mcp_native_link,
    read_mcp_server_row, read_mcp_server_rows, set_mcp_server_selection, update_mcp_server_catalog,
    upsert_mcp_native_link, McpServerCatalogValues, McpServerFields,
    McpServerRow as SharedMcpServerRow, McpServerWriteOutcome, SharedDatabase, SharedStoreError,
};
use hmac::{Hmac, Mac};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;

const SAFE_JS_INTEGER_MASK: u64 = (1_u64 << 53) - 1;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("shared MCP database failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    SharedStore(SharedStoreError),
    #[error("shared MCP data is invalid: {0}")]
    InvalidStore(String),
    #[error("MCP server is invalid: {0}")]
    InvalidServer(String),
    #[error("MCP server '{0}' was not found")]
    NotFound(String),
    #[error("MCP server changed outside this editor; reload and try again")]
    Conflict,
    #[error("shared MCP update failed and live recovery was incomplete: {0}")]
    Recovery(String),
}

impl McpError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_) | Self::SharedStore(_) => "storage_error",
            Self::InvalidStore(_) => "invalid_store",
            Self::InvalidServer(_) => "invalid_mcp_server",
            Self::NotFound(_) => "not_found",
            Self::Conflict => "conflict",
            Self::Recovery(_) => "recovery_failed",
        }
    }
}

impl From<SharedStoreError> for McpError {
    fn from(error: SharedStoreError) -> Self {
        match error {
            SharedStoreError::Database(error) => Self::Database(error),
            SharedStoreError::InvalidDatabase(message) => Self::InvalidStore(message),
            other => Self::SharedStore(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct McpApps(BTreeMap<String, bool>);

impl Default for McpApps {
    fn default() -> Self {
        Self(
            builtin_app_registry()
                .descriptors()
                .filter(|descriptor| descriptor.mcp_contract().is_some())
                .map(|descriptor| (descriptor.id().to_owned(), false))
                .collect(),
        )
    }
}

impl McpApps {
    pub(crate) fn enabled(&self, app: &AppType) -> bool {
        self.0.get(app.as_str()).copied().unwrap_or(false)
    }

    fn set(&mut self, app: &AppType, enabled: bool) -> Result<(), McpError> {
        require_mcp_app(app)?;
        self.0.insert(app.as_str().to_owned(), enabled);
        Ok(())
    }

    fn from_row(row: &SharedMcpServerRow) -> Self {
        let mut apps = Self::default();
        for descriptor in builtin_app_registry().descriptors() {
            if let Some(enabled) = row.enabled_for(descriptor.app()) {
                apps.0.insert(descriptor.id().to_owned(), enabled);
            }
        }
        apps
    }

    fn validate(&self) -> Result<(), McpError> {
        for id in self.0.keys() {
            let descriptor = builtin_app_registry()
                .find(id)
                .filter(|descriptor| descriptor.id() == id && descriptor.mcp_contract().is_some());
            if descriptor.is_none() {
                return Err(McpError::InvalidServer(format!(
                    "application '{id}' does not support MCP"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub server: Value,
    #[serde(default)]
    pub apps: McpApps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub revision: u64,
}

impl fmt::Debug for McpServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServer")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("server", &"<redacted>")
            .field("apps", &self.apps)
            .field("description", &self.description)
            .field("homepage", &self.homepage)
            .field("docs", &self.docs)
            .field("tags", &self.tags)
            .field("revision", &self.revision)
            .finish()
    }
}

#[derive(Clone)]
pub enum McpLiveChange {
    Upsert {
        app: AppType,
        id: String,
        previous: Option<Value>,
        server: Value,
        native_snapshot: Option<McpNativeSnapshot>,
        link_state: McpNativeLinkState,
    },
    Disable {
        app: AppType,
        id: String,
        previous: Value,
        server: Value,
        native_snapshot: Option<McpNativeSnapshot>,
        link_state: McpNativeLinkState,
    },
    Remove {
        app: AppType,
        id: String,
        server: Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpNativeLinkState {
    Unowned,
    Owned,
    Observed,
}

impl McpLiveChange {
    pub fn app(&self) -> &AppType {
        match self {
            Self::Upsert { app, .. } | Self::Disable { app, .. } | Self::Remove { app, .. } => app,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Upsert { id, .. } | Self::Disable { id, .. } | Self::Remove { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpImportReport {
    pub new_servers: usize,
    pub enabled_apps: usize,
    pub disabled_apps: usize,
    pub failed_apps: Vec<String>,
}

pub type McpImportsByApp = Vec<(AppType, Result<Vec<cc_switch_core::McpImport>, String>)>;

pub struct McpStore {
    database: SharedDatabase,
    revision_key: [u8; 16],
}

struct StoredMcpServer {
    server: McpServer,
    source_fingerprint: [u8; 32],
}

impl McpStore {
    pub fn open(path: PathBuf) -> Result<Self, McpError> {
        let store = Self {
            database: SharedDatabase::open(path)?,
            revision_key: *Uuid::new_v4().as_bytes(),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn list(&self) -> Result<Vec<McpServer>, McpError> {
        let connection = self.connect()?;
        read_mcp_server_rows(&connection)?
            .into_iter()
            .map(|row| row_to_server(row, &self.revision_key))
            .collect()
    }

    pub fn upsert_with_live<T, E>(
        &self,
        server: McpServer,
        apply: impl FnOnce(&mut [McpLiveChange]) -> Result<T, E>,
        rollback: impl FnOnce(T) -> Result<(), String>,
    ) -> Result<Result<(), E>, McpError> {
        validate_server(&server)?;
        let mut connection = self.connect()?;
        let mut transaction = begin_immediate_transaction(&mut connection)?;
        let current = get_server(&transaction, &server.id, &self.revision_key)?;
        let current_server = current.as_ref().map(|stored| &stored.server);
        ensure_expected_revision(current_server, server.revision)?;
        let mut changes = live_changes(&transaction, current_server, Some(&server))?;
        let receipt = match apply(&mut changes) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let finalize = (|| -> Result<(), McpError> {
            write_server(
                &mut transaction,
                current.as_ref().map(|stored| &stored.source_fingerprint),
                &server,
            )?;
            persist_native_links(&mut transaction, &changes)?;
            transaction.commit()?;
            Ok(())
        })();
        recover_on_failure(finalize, receipt, rollback)?;
        Ok(Ok(()))
    }

    pub fn toggle_with_live<T, E>(
        &self,
        id: &str,
        expected_revision: u64,
        app: AppType,
        enabled: bool,
        apply: impl FnOnce(&mut [McpLiveChange]) -> Result<T, E>,
        rollback: impl FnOnce(T) -> Result<(), String>,
    ) -> Result<Result<(), E>, McpError> {
        let target = require_mcp_app(&app)?;
        let mut connection = self.connect()?;
        let mut transaction = begin_immediate_transaction(&mut connection)?;
        let current = get_server(&transaction, id, &self.revision_key)?
            .ok_or_else(|| McpError::NotFound(id.into()))?;
        ensure_expected_revision(Some(&current.server), expected_revision)?;
        if current.server.apps.enabled(&app) == enabled {
            transaction.commit()?;
            return Ok(Ok(()));
        }
        let mut updated = current.server.clone();
        updated.apps.set(&app, enabled)?;
        validate_server(&updated)?;
        let mut changes = live_changes(&transaction, Some(&current.server), Some(&updated))?;
        let receipt = match apply(&mut changes) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let finalize = (|| -> Result<(), McpError> {
            persist_native_links(&mut transaction, &changes)?;
            require_applied(set_mcp_server_selection(
                &mut transaction,
                id,
                &current.source_fingerprint,
                target,
                enabled,
            )?)?;
            transaction.commit()?;
            Ok(())
        })();
        recover_on_failure(finalize, receipt, rollback)?;
        Ok(Ok(()))
    }

    pub fn delete_with_live<T, E>(
        &self,
        id: &str,
        expected_revision: u64,
        apply: impl FnOnce(&mut [McpLiveChange]) -> Result<T, E>,
        rollback: impl FnOnce(T) -> Result<(), String>,
    ) -> Result<Result<(), E>, McpError> {
        let mut connection = self.connect()?;
        let mut transaction = begin_immediate_transaction(&mut connection)?;
        let current = get_server(&transaction, id, &self.revision_key)?
            .ok_or_else(|| McpError::NotFound(id.into()))?;
        ensure_expected_revision(Some(&current.server), expected_revision)?;
        let mut changes = live_changes(&transaction, Some(&current.server), None)?;
        let receipt = match apply(&mut changes) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let finalize = (|| -> Result<(), McpError> {
            delete_mcp_native_links(&mut transaction, id)?;
            require_applied(delete_mcp_server(
                &mut transaction,
                id,
                &current.source_fingerprint,
            )?)?;
            transaction.commit()?;
            Ok(())
        })();
        recover_on_failure(finalize, receipt, rollback)?;
        Ok(Ok(()))
    }

    pub fn import_with_live<T, E>(
        &self,
        observe: impl FnOnce() -> Result<(McpImportsByApp, T), E>,
        verify_live_snapshot: impl FnOnce(&T) -> bool,
    ) -> Result<Result<McpImportReport, E>, McpError> {
        let mut connection = self.connect()?;
        let mut transaction = begin_immediate_transaction(&mut connection)?;
        let (imports, observation) = match observe() {
            Ok(observed) => observed,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let report = match merge_imports(&mut transaction, imports, &self.revision_key) {
            Ok(report) => report,
            Err(error) => {
                transaction.rollback()?;
                drop(observation);
                return Err(error);
            }
        };
        if !verify_live_snapshot(&observation) {
            transaction.rollback()?;
            drop(observation);
            return Err(McpError::Conflict);
        }
        transaction.commit()?;
        drop(observation);
        Ok(Ok(report))
    }

    fn initialize(&self) -> Result<(), McpError> {
        self.database.ensure_mcp_server_schema()?;
        let mut connection = self.connect()?;
        ensure_mcp_native_link_schema(&mut connection)?;
        Ok(())
    }

    fn connect(&self) -> Result<Connection, McpError> {
        self.database.connect().map_err(McpError::from)
    }
}

fn merge_imports(
    transaction: &mut rusqlite::Transaction<'_>,
    imports: McpImportsByApp,
    revision_key: &[u8; 16],
) -> Result<McpImportReport, McpError> {
    let mut report = McpImportReport::default();
    for (app, imports) in imports {
        let target = require_mcp_app(&app)?;
        let imports = match imports {
            Ok(imports) => imports,
            Err(error) => {
                report
                    .failed_apps
                    .push(format!("{}: {error}", app.as_str()));
                continue;
            }
        };
        for import in imports {
            if let Some(mut current) = get_server(transaction, &import.id, revision_key)? {
                if !mcp_servers_equivalent(&app, &current.server.server, &import.server) {
                    report.failed_apps.push(format!(
                        "{}: server '{}' conflicts with the shared catalog",
                        app.as_str(),
                        import.id
                    ));
                    continue;
                }
                if import.enabled {
                    if let Err(error) =
                        validate_app_activation(&app, &current.server.id, &current.server.server)
                    {
                        report
                            .failed_apps
                            .push(format!("{}: {error}", app.as_str()));
                        continue;
                    }
                }
                if current.server.apps.enabled(&app) != import.enabled {
                    current.server.apps.set(&app, import.enabled)?;
                    require_applied(set_mcp_server_selection(
                        transaction,
                        &import.id,
                        &current.source_fingerprint,
                        target,
                        import.enabled,
                    )?)?;
                    if import.enabled {
                        report.enabled_apps += 1;
                    } else {
                        report.disabled_apps += 1;
                    }
                }
            } else {
                let mut apps = McpApps::default();
                apps.set(&app, import.enabled)?;
                let server = McpServer {
                    id: import.id.clone(),
                    name: import.id.clone(),
                    server: import.server,
                    apps,
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: Vec::new(),
                    revision: 0,
                };
                validate_server(&server)?;
                write_server(transaction, None, &server)?;
                report.new_servers += 1;
            }
            upsert_native_link(
                transaction,
                &import.id,
                &app,
                import.native_snapshot.as_ref(),
            )?;
        }
    }
    Ok(report)
}

fn row_to_server(row: SharedMcpServerRow, revision_key: &[u8; 16]) -> Result<McpServer, McpError> {
    let server = serde_json::from_str(&row.server_config).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let tags = serde_json::from_str(&row.tags).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let revision = server_revision(row.source_fingerprint(), revision_key);
    let apps = McpApps::from_row(&row);
    Ok(McpServer {
        id: row.id,
        name: row.name,
        server,
        description: row.description,
        homepage: row.homepage,
        docs: row.docs,
        tags,
        revision,
        apps,
    })
}

fn get_server(
    connection: &Connection,
    id: &str,
    revision_key: &[u8; 16],
) -> Result<Option<StoredMcpServer>, McpError> {
    read_mcp_server_row(connection, id)?
        .map(|row| {
            let source_fingerprint = *row.source_fingerprint();
            row_to_server(row, revision_key).map(|server| StoredMcpServer {
                server,
                source_fingerprint,
            })
        })
        .transpose()
}

fn ensure_expected_revision(
    current: Option<&McpServer>,
    expected_revision: u64,
) -> Result<(), McpError> {
    match current {
        Some(server) if expected_revision == server.revision => Ok(()),
        None if expected_revision == 0 => Ok(()),
        _ => Err(McpError::Conflict),
    }
}

fn server_revision(source_fingerprint: &[u8; 32], key: &[u8; 16]) -> u64 {
    let mut hasher = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 16-byte key");
    hasher.update(source_fingerprint);
    let bytes = hasher.finalize().into_bytes();
    let revision = u64::from_be_bytes(bytes[..8].try_into().expect("SHA-256 has eight bytes"));
    (revision & SAFE_JS_INTEGER_MASK).max(1)
}

fn write_server(
    transaction: &mut rusqlite::Transaction<'_>,
    expected_fingerprint: Option<&[u8; 32]>,
    server: &McpServer,
) -> Result<(), McpError> {
    let server_config = serde_json::to_string(&server.server)
        .map_err(|error| McpError::InvalidServer(error.to_string()))?;
    let tags = serde_json::to_string(&server.tags)
        .map_err(|error| McpError::InvalidServer(error.to_string()))?;
    let values = McpServerCatalogValues::new(
        McpServerFields {
            id: &server.id,
            name: &server.name,
            server_config: &server_config,
            description: server.description.as_deref(),
            homepage: server.homepage.as_deref(),
            docs: server.docs.as_deref(),
            tags: &tags,
        },
        |app| server.apps.enabled(app),
    );
    let outcome = match expected_fingerprint {
        Some(expected) => update_mcp_server_catalog(transaction, expected, &values)?,
        None => insert_mcp_server_catalog(transaction, &values)?,
    };
    require_applied(outcome)
}

fn require_applied(outcome: McpServerWriteOutcome) -> Result<(), McpError> {
    match outcome {
        McpServerWriteOutcome::Applied => Ok(()),
        McpServerWriteOutcome::NotApplied => Err(McpError::Conflict),
    }
}

fn validate_server(server: &McpServer) -> Result<(), McpError> {
    validate_mcp_server(&server.id, &server.server)
        .map_err(|error| McpError::InvalidServer(error.to_string()))?;
    server.apps.validate()?;
    if server.name.trim().is_empty() || server.name.len() > 128 {
        return Err(McpError::InvalidServer(
            "name must contain at most 128 bytes".to_owned(),
        ));
    }
    for app in AppType::all() {
        if server.apps.enabled(&app) {
            validate_app_activation(&app, &server.id, &server.server)?;
        }
    }
    if server.tags.len() > 32
        || server
            .tags
            .iter()
            .any(|tag| tag.trim().is_empty() || tag.len() > 64)
    {
        return Err(McpError::InvalidServer("tags are invalid".to_owned()));
    }
    for value in [
        server.description.as_deref(),
        server.homepage.as_deref(),
        server.docs.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.len() > 2048 || value.contains('\0') {
            return Err(McpError::InvalidServer(
                "metadata is too large or contains NUL".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_app_activation(app: &AppType, id: &str, server: &Value) -> Result<(), McpError> {
    require_mcp_app(app)?;
    validate_mcp_server_for_app(app, id, server)
        .map_err(|error| McpError::InvalidServer(error.to_string()))
}

fn require_mcp_app(app: &AppType) -> Result<McpCatalogColumn, McpError> {
    let descriptor = builtin_app_registry().for_app(app);
    descriptor
        .mcp_contract()
        .map(|contract| contract.catalog_column())
        .ok_or_else(|| {
            McpError::InvalidServer(format!(
                "application '{}' does not support MCP",
                app.as_str()
            ))
        })
}

fn live_changes(
    transaction: &rusqlite::Transaction<'_>,
    before: Option<&McpServer>,
    after: Option<&McpServer>,
) -> Result<Vec<McpLiveChange>, McpError> {
    let mut changes = Vec::new();
    for descriptor in builtin_app_registry().descriptors() {
        let Some(contract) = descriptor.mcp_contract() else {
            continue;
        };
        let app = descriptor.app().clone();
        let preserves_disabled_entry = contract.preserves_disabled_entry();
        let id = after
            .or(before)
            .expect("a live change has a server")
            .id
            .as_str();
        let native_link = get_native_link(transaction, id, &app)?;
        let link_state = if native_link.is_some() {
            McpNativeLinkState::Owned
        } else {
            McpNativeLinkState::Unowned
        };
        let native_snapshot = native_link.flatten();
        let was_enabled = before.is_some_and(|server| server.apps.enabled(&app));
        let is_enabled = after.is_some_and(|server| server.apps.enabled(&app));
        match (before, after, was_enabled, is_enabled) {
            (previous, Some(server), _, true)
                if !was_enabled
                    || previous.is_none_or(|current| current.server != server.server) =>
            {
                changes.push(McpLiveChange::Upsert {
                    app,
                    id: server.id.clone(),
                    previous: previous.map(|server| server.server.clone()),
                    server: server.server.clone(),
                    native_snapshot,
                    link_state,
                });
            }
            (Some(previous), Some(server), _, false)
                if was_enabled
                    || (preserves_disabled_entry
                        && link_state == McpNativeLinkState::Owned
                        && previous.server != server.server) =>
            {
                changes.push(McpLiveChange::Disable {
                    app,
                    id: server.id.clone(),
                    previous: previous.server.clone(),
                    server: server.server.clone(),
                    native_snapshot,
                    link_state,
                });
            }
            (Some(previous), None, _, _)
                if was_enabled
                    || (preserves_disabled_entry && link_state == McpNativeLinkState::Owned) =>
            {
                changes.push(McpLiveChange::Remove {
                    app,
                    id: previous.id.clone(),
                    server: previous.server.clone(),
                });
            }
            _ => {}
        }
    }
    Ok(changes)
}

fn get_native_link(
    connection: &Connection,
    server_id: &str,
    app: &AppType,
) -> Result<Option<Option<McpNativeSnapshot>>, McpError> {
    read_mcp_native_link(connection, server_id, app.as_str())?
        .map(|link| {
            link.native_snapshot
                .map(|raw| {
                    serde_json::from_str(&raw)
                        .map_err(|error| McpError::InvalidStore(error.to_string()))
                })
                .transpose()
        })
        .transpose()
}

fn persist_native_links(
    transaction: &mut rusqlite::Transaction<'_>,
    changes: &[McpLiveChange],
) -> Result<(), McpError> {
    for change in changes {
        match change {
            McpLiveChange::Upsert {
                app,
                id,
                link_state: McpNativeLinkState::Observed,
                ..
            } => upsert_native_link(transaction, id, app, None)?,
            McpLiveChange::Upsert { .. } => {}
            McpLiveChange::Disable {
                app,
                id,
                native_snapshot,
                link_state,
                ..
            } if *link_state != McpNativeLinkState::Unowned => {
                upsert_native_link(transaction, id, app, native_snapshot.as_ref())?;
            }
            McpLiveChange::Disable { .. } => {}
            McpLiveChange::Remove { .. } => {}
        }
    }
    Ok(())
}

fn upsert_native_link(
    transaction: &mut rusqlite::Transaction<'_>,
    server_id: &str,
    app: &AppType,
    snapshot: Option<&McpNativeSnapshot>,
) -> Result<(), McpError> {
    let snapshot = snapshot
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| McpError::InvalidStore(error.to_string()))?;
    upsert_mcp_native_link(transaction, server_id, app.as_str(), snapshot.as_deref())?;
    Ok(())
}

fn recover_on_failure<T>(
    result: Result<(), McpError>,
    receipt: T,
    rollback: impl FnOnce(T) -> Result<(), String>,
) -> Result<(), McpError> {
    if let Err(error) = result {
        if let Err(rollback_error) = rollback(receipt) {
            return Err(McpError::Recovery(format!(
                "database error: {error}; live recovery error: {rollback_error}"
            )));
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn apps_enabled(apps: impl IntoIterator<Item = AppType>) -> McpApps {
        let mut selections = McpApps::default();
        for app in apps {
            selections.set(&app, true).unwrap();
        }
        selections
    }

    fn server() -> McpServer {
        McpServer {
            id: "context7".to_owned(),
            name: "Context7".to_owned(),
            server: json!({"type":"stdio","command":"npx","future":true}),
            apps: apps_enabled([AppType::Claude]),
            description: Some("Docs".to_owned()),
            homepage: None,
            docs: None,
            tags: vec!["docs".to_owned()],
            revision: 0,
        }
    }

    fn import_observed(
        store: &McpStore,
        imports: McpImportsByApp,
        snapshot_is_current: bool,
    ) -> Result<McpImportReport, McpError> {
        store.import_with_live(|| Ok::<_, McpError>((imports, ())), |_| snapshot_is_current)?
    }

    #[test]
    fn shared_schema_round_trips_servers() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        store
            .upsert_with_live(server(), |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_ne!(listed[0].revision, 0);
        let mut expected = server();
        expected.revision = listed[0].revision;
        assert_eq!(listed, vec![expected]);
    }

    #[test]
    fn shared_schema_and_ipc_apps_follow_the_core_registry() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let registry_apps = builtin_app_registry()
            .descriptors()
            .filter(|descriptor| descriptor.mcp_contract().is_some())
            .map(|descriptor| descriptor.app().clone())
            .collect::<Vec<_>>();
        let mut record = server();
        record.apps = apps_enabled(registry_apps.clone());

        store
            .upsert_with_live(record, |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        let current = store.list().unwrap().remove(0);
        for app in &registry_apps {
            assert!(current.apps.enabled(app));
        }

        let serialized = serde_json::to_value(current.apps).unwrap();
        let keys = serialized
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let expected = registry_apps
            .iter()
            .map(|app| app.as_str().to_owned())
            .collect();
        assert_eq!(keys, expected);
    }

    #[test]
    fn save_rejects_non_mcp_app_selections() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let mut record = server();
        record.apps.0.insert("pi".to_owned(), true);

        let result = store.upsert_with_live(record, |_| Ok::<_, ()>(()), |_| Ok(()));

        assert!(matches!(result, Err(McpError::InvalidServer(_))));
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn first_ownership_migration_claims_legacy_enabled_rows() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let database = SharedDatabase::open(path.clone()).unwrap();
        database.ensure_mcp_server_schema().unwrap();
        let mut connection = database.connect().unwrap();
        let mut transaction = begin_immediate_transaction(&mut connection).unwrap();
        write_server(&mut transaction, None, &server()).unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'mcp_native_links'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        drop(connection);
        drop(database);

        let store = McpStore::open(path).unwrap();
        let connection = store.connect().unwrap();
        assert!(get_native_link(&connection, "context7", &AppType::Claude)
            .unwrap()
            .is_some());
    }

    #[test]
    fn existing_ownership_table_does_not_claim_unlinked_rows() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let database = SharedDatabase::open(path.clone()).unwrap();
        database.ensure_mcp_server_schema().unwrap();
        let mut connection = database.connect().unwrap();
        ensure_mcp_native_link_schema(&mut connection).unwrap();
        let mut transaction = begin_immediate_transaction(&mut connection).unwrap();
        write_server(&mut transaction, None, &server()).unwrap();
        transaction.commit().unwrap();
        drop(connection);
        drop(database);

        let store = McpStore::open(path).unwrap();
        let connection = store.connect().unwrap();
        assert!(get_native_link(&connection, "context7", &AppType::Claude)
            .unwrap()
            .is_none());
    }

    #[test]
    fn upsert_preserves_unknown_future_columns() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = McpStore::open(path.clone()).unwrap();
        store
            .upsert_with_live(server(), |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE mcp_servers ADD COLUMN future TEXT NOT NULL DEFAULT 'keep';",
            )
            .unwrap();
        let update = store.list().unwrap().remove(0);
        store
            .upsert_with_live(update, |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT future FROM mcp_servers WHERE id='context7'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "keep"
        );
    }

    #[test]
    fn live_failure_leaves_database_unchanged() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let result = store
            .upsert_with_live(server(), |_| Err::<(), _>("live failed"), |_| Ok(()))
            .unwrap();
        assert_eq!(result, Err("live failed"));
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn suppressed_core_write_is_a_conflict_and_rolls_back_live() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        store
            .connect()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER suppress_mcp BEFORE INSERT ON mcp_servers BEGIN
                    SELECT RAISE(IGNORE);
                 END;",
            )
            .unwrap();
        let applied = std::cell::Cell::new(false);
        let rolled_back = std::cell::Cell::new(false);

        let result = store.upsert_with_live(
            server(),
            |_| {
                applied.set(true);
                Ok::<_, ()>(())
            },
            |_| {
                rolled_back.set(true);
                Ok(())
            },
        );

        assert!(matches!(result, Err(McpError::Conflict)));
        assert!(applied.get());
        assert!(rolled_back.get());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn external_database_edit_rejects_a_stale_whole_row_update() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = McpStore::open(path.clone()).unwrap();
        store
            .upsert_with_live(server(), |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        let mut stale = store.list().unwrap().remove(0);
        Connection::open(path)
            .unwrap()
            .execute(
                "UPDATE mcp_servers SET enabled_codex = 1 WHERE id = 'context7'",
                [],
            )
            .unwrap();
        stale.name = "Stale edit".to_owned();
        let applied = std::cell::Cell::new(false);
        let result = store.upsert_with_live(
            stale,
            |_| {
                applied.set(true);
                Ok::<_, ()>(())
            },
            |_| Ok(()),
        );
        assert!(matches!(result, Err(McpError::Conflict)));
        assert!(!applied.get());
        let current = store.list().unwrap().remove(0);
        assert_eq!(current.name, "Context7");
        assert!(current.apps.enabled(&AppType::Codex));
    }

    #[test]
    fn external_extension_edit_rejects_a_stale_update() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = McpStore::open(path.clone()).unwrap();
        store
            .upsert_with_live(server(), |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "ALTER TABLE mcp_servers ADD COLUMN host_extension TEXT;
                 UPDATE mcp_servers SET host_extension = 'first' WHERE id = 'context7';",
            )
            .unwrap();
        let mut stale = store.list().unwrap().remove(0);
        Connection::open(path)
            .unwrap()
            .execute(
                "UPDATE mcp_servers SET host_extension = 'second' WHERE id = 'context7'",
                [],
            )
            .unwrap();
        stale.name = "Stale edit".to_owned();
        let applied = std::cell::Cell::new(false);

        let result = store.upsert_with_live(
            stale,
            |_| {
                applied.set(true);
                Ok::<_, ()>(())
            },
            |_| Ok(()),
        );

        assert!(matches!(result, Err(McpError::Conflict)));
        assert!(!applied.get());
        assert_eq!(store.list().unwrap().remove(0).name, "Context7");
    }

    #[test]
    fn external_database_edit_rejects_a_stale_toggle() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = McpStore::open(path.clone()).unwrap();
        store
            .upsert_with_live(server(), |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        let stale_revision = store.list().unwrap().remove(0).revision;
        Connection::open(path)
            .unwrap()
            .execute(
                "UPDATE mcp_servers SET server_config = ?1 WHERE id = 'context7'",
                [r#"{"type":"stdio","command":"uvx"}"#],
            )
            .unwrap();
        let applied = std::cell::Cell::new(false);

        let result = store.toggle_with_live(
            "context7",
            stale_revision,
            AppType::Codex,
            true,
            |_| {
                applied.set(true);
                Ok::<_, ()>(())
            },
            |_| Ok(()),
        );

        assert!(matches!(result, Err(McpError::Conflict)));
        assert!(!applied.get());
        assert!(!store
            .list()
            .unwrap()
            .remove(0)
            .apps
            .enabled(&AppType::Codex));
    }

    #[test]
    fn import_reports_same_id_connection_conflicts_without_merging_flags() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let report = import_observed(
            &store,
            vec![
                (
                    AppType::Claude,
                    Ok(vec![cc_switch_core::McpImport {
                        id: "shared".to_owned(),
                        server: json!({"type":"stdio","command":"npx"}),
                        enabled: true,
                        native_snapshot: None,
                    }]),
                ),
                (
                    AppType::Codex,
                    Ok(vec![cc_switch_core::McpImport {
                        id: "shared".to_owned(),
                        server: json!({"type":"stdio","command":"uvx"}),
                        enabled: true,
                        native_snapshot: None,
                    }]),
                ),
            ],
            true,
        )
        .unwrap();
        assert_eq!(report.new_servers, 1);
        assert_eq!(report.failed_apps.len(), 1);
        let current = store.list().unwrap().remove(0);
        assert_eq!(current.server["command"], "npx");
        assert!(current.apps.enabled(&AppType::Claude));
        assert!(!current.apps.enabled(&AppType::Codex));
    }

    #[test]
    fn import_keeps_native_disabled_state_until_explicitly_enabled() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let import = |enabled| {
            vec![(
                AppType::OpenCode,
                Ok(vec![cc_switch_core::McpImport {
                    id: "local".to_owned(),
                    server: json!({"type":"stdio","command":"npx"}),
                    enabled,
                    native_snapshot: None,
                }]),
            )]
        };
        import_observed(&store, import(false), true).unwrap();
        assert!(!store
            .list()
            .unwrap()
            .remove(0)
            .apps
            .enabled(&AppType::OpenCode));
        let connection = store.connect().unwrap();
        assert!(get_native_link(&connection, "local", &AppType::OpenCode)
            .unwrap()
            .is_some());

        let report = import_observed(&store, import(true), true).unwrap();
        assert_eq!(report.enabled_apps, 1);
        assert!(store
            .list()
            .unwrap()
            .remove(0)
            .apps
            .enabled(&AppType::OpenCode));
    }

    #[test]
    fn import_rejects_activation_the_target_cannot_represent() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let mut record = server();
        record.server = json!({"type":"stdio","command":"npx","cwd":"/repo"});
        record.apps = McpApps::default();
        store
            .upsert_with_live(record, |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();

        let report = import_observed(
            &store,
            vec![(
                AppType::OpenCode,
                Ok(vec![cc_switch_core::McpImport {
                    id: "context7".to_owned(),
                    server: json!({"type":"stdio","command":"npx"}),
                    enabled: true,
                    native_snapshot: None,
                }]),
            )],
            true,
        )
        .unwrap();

        assert_eq!(report.enabled_apps, 0);
        assert_eq!(report.failed_apps.len(), 1);
        assert!(!store
            .list()
            .unwrap()
            .remove(0)
            .apps
            .enabled(&AppType::OpenCode));
        assert!(
            get_native_link(&store.connect().unwrap(), "context7", &AppType::OpenCode)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn core_registry_and_mcp_selections_stay_aligned() {
        let mut columns = Vec::new();
        for descriptor in builtin_app_registry().descriptors() {
            match descriptor.mcp_contract() {
                Some(contract) => {
                    let column = require_mcp_app(descriptor.app()).unwrap();
                    assert_eq!(column, contract.catalog_column());
                    columns.push(column);
                }
                None => assert!(require_mcp_app(descriptor.app()).is_err()),
            }
        }
        assert_eq!(
            columns,
            cc_switch_core::mcp_catalog_columns().collect::<Vec<_>>()
        );
    }

    #[test]
    fn disabled_unowned_native_entries_are_never_mutated() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let mut connection = store.connect().unwrap();
        let mut transaction = connection.transaction().unwrap();
        let mut previous = server();
        previous.apps = McpApps::default();
        let mut updated = previous.clone();
        updated.server = json!({"type":"stdio","command":"uvx"});
        write_server(&mut transaction, None, &previous).unwrap();

        assert!(live_changes(&transaction, Some(&previous), Some(&updated))
            .unwrap()
            .is_empty());
        assert!(live_changes(&transaction, Some(&previous), None)
            .unwrap()
            .is_empty());

        upsert_native_link(&mut transaction, &previous.id, &AppType::Claude, None).unwrap();
        assert!(live_changes(&transaction, Some(&previous), Some(&updated))
            .unwrap()
            .is_empty());
        assert!(live_changes(&transaction, Some(&previous), None)
            .unwrap()
            .is_empty());

        upsert_native_link(&mut transaction, &previous.id, &AppType::OpenCode, None).unwrap();
        let removals = live_changes(&transaction, Some(&previous), None).unwrap();
        assert_eq!(removals.len(), 1);
        assert!(matches!(
            &removals[0],
            McpLiveChange::Remove {
                app: AppType::OpenCode,
                ..
            }
        ));
    }

    #[test]
    fn deleting_a_disabled_removable_link_forgets_ownership_without_live_removal() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = McpStore::open(path.clone()).unwrap();
        let mut record = server();
        record.apps = McpApps::default();
        store
            .upsert_with_live(record, |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        let mut connection = Connection::open(&path).unwrap();
        let mut transaction = connection.transaction().unwrap();
        upsert_native_link(&mut transaction, "context7", &AppType::Gemini, None).unwrap();
        transaction.commit().unwrap();
        let current = store.list().unwrap().remove(0);

        store
            .delete_with_live(
                &current.id,
                current.revision,
                |changes| {
                    assert!(changes.is_empty());
                    Ok::<_, ()>(())
                },
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();

        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM mcp_native_links WHERE server_id='context7'",
                    [],
                    |row| row.get::<_, usize>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn owned_disabled_native_entries_track_catalog_edits() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let mut connection = store.connect().unwrap();
        let mut transaction = connection.transaction().unwrap();
        let mut previous = server();
        previous.apps = McpApps::default();
        let mut updated = previous.clone();
        updated.server = json!({"type":"stdio","command":"uvx"});
        write_server(&mut transaction, None, &previous).unwrap();
        upsert_native_link(&mut transaction, &previous.id, &AppType::OpenCode, None).unwrap();

        let updates = live_changes(&transaction, Some(&previous), Some(&updated)).unwrap();

        assert_eq!(updates.len(), 1);
        assert!(matches!(
            &updates[0],
            McpLiveChange::Disable {
                app: AppType::OpenCode,
                previous: value,
                server,
                link_state: McpNativeLinkState::Owned,
                ..
            } if value["command"] == "npx" && server["command"] == "uvx"
        ));
    }

    #[test]
    fn unobserved_live_changes_do_not_create_native_ownership() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = McpStore::open(path.clone()).unwrap();
        let mut record = server();
        record.apps = apps_enabled([AppType::Gemini]);
        store
            .upsert_with_live(record, |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        let connection = Connection::open(&path).unwrap();
        let link_count = || {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM mcp_native_links WHERE server_id='context7' AND app_id='gemini'",
                    [],
                    |row| row.get::<_, usize>(0),
                )
                .unwrap()
        };
        assert_eq!(link_count(), 0);

        let current = store.list().unwrap().remove(0);
        store
            .toggle_with_live(
                &current.id,
                current.revision,
                AppType::Gemini,
                false,
                |_| Ok::<_, ()>(()),
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();

        assert_eq!(link_count(), 0);
    }

    #[test]
    fn native_snapshot_is_transactional_link_state() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = McpStore::open(path.clone()).unwrap();
        let mut record = server();
        record.apps = apps_enabled([AppType::Gemini]);
        store
            .upsert_with_live(
                record,
                |changes| {
                    let McpLiveChange::Upsert { link_state, .. } = &mut changes[0] else {
                        panic!("expected upsert");
                    };
                    *link_state = McpNativeLinkState::Observed;
                    Ok::<_, ()>(())
                },
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();
        let snapshot = cc_switch_core::import_mcp_servers(
            &AppType::Gemini,
            Some(br#"{"mcpServers":{"context7":{"command":"npx","timeout":30}}}"#),
        )
        .unwrap()
        .remove(0)
        .native_snapshot
        .unwrap();

        let current = store.list().unwrap().remove(0);
        store
            .toggle_with_live(
                &current.id,
                current.revision,
                AppType::Gemini,
                false,
                |changes| {
                    let McpLiveChange::Disable {
                        native_snapshot, ..
                    } = &mut changes[0]
                    else {
                        panic!("expected disable");
                    };
                    *native_snapshot = Some(snapshot.clone());
                    Ok::<_, ()>(())
                },
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();
        let connection = Connection::open(&path).unwrap();
        assert!(connection
            .query_row(
                "SELECT native_snapshot FROM mcp_native_links WHERE server_id='context7' AND app_id='gemini'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
            .is_some());

        let current = store.list().unwrap().remove(0);
        store
            .toggle_with_live(
                &current.id,
                current.revision,
                AppType::Gemini,
                true,
                |changes| {
                    let McpLiveChange::Upsert {
                        native_snapshot,
                        link_state,
                        ..
                    } = &mut changes[0]
                    else {
                        panic!("expected upsert");
                    };
                    assert!(native_snapshot.is_some());
                    *link_state = McpNativeLinkState::Observed;
                    Ok::<_, ()>(())
                },
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();
        assert!(connection
            .query_row(
                "SELECT native_snapshot FROM mcp_native_links WHERE server_id='context7' AND app_id='gemini'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn enabling_an_app_rejects_fields_its_native_format_cannot_express() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let mut record = server();
        record.server = json!({"type":"stdio","command":"npx","cwd":"/repo"});
        store
            .upsert_with_live(record, |_| Ok::<_, ()>(()), |_| Ok(()))
            .unwrap()
            .unwrap();
        let current = store.list().unwrap().remove(0);
        let applied = std::cell::Cell::new(false);

        let result = store.toggle_with_live(
            &current.id,
            current.revision,
            AppType::OpenCode,
            true,
            |_| {
                applied.set(true);
                Ok::<_, ()>(())
            },
            |_| Ok(()),
        );

        assert!(matches!(result, Err(McpError::InvalidServer(_))));
        assert!(!applied.get());
    }

    #[test]
    fn import_rolls_back_when_the_live_snapshot_changes() {
        let directory = tempdir().unwrap();
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        let imports = vec![(
            AppType::Claude,
            Ok(vec![cc_switch_core::McpImport {
                id: "context7".to_owned(),
                server: json!({"command":"npx"}),
                enabled: true,
                native_snapshot: None,
            }]),
        )];

        let error = import_observed(&store, imports, false).expect_err("changed live snapshot");

        assert!(matches!(error, McpError::Conflict));
        assert!(store.list().unwrap().is_empty());
    }
}
