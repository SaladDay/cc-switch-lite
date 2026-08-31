use std::{collections::HashSet, fmt, path::PathBuf, time::Duration};

use cc_switch_core::{
    builtin_app_adapter, builtin_app_registry, mcp_servers_equivalent, validate_mcp_server,
    AppCapability, AppType,
};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SAFE_JS_INTEGER_MASK: u64 = (1_u64 << 53) - 1;
const MCP_SELECT: &str = "SELECT id, name, server_config, description, homepage, docs, tags,
    enabled_claude, enabled_codex, enabled_gemini, enabled_grokbuild, enabled_opencode,
    enabled_hermes FROM mcp_servers";

#[derive(Debug, Error)]
pub enum McpError {
    #[error("shared MCP database failed: {0}")]
    Database(#[from] rusqlite::Error),
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
            Self::Database(_) => "storage_error",
            Self::InvalidStore(_) => "invalid_store",
            Self::InvalidServer(_) => "invalid_mcp_server",
            Self::NotFound(_) => "not_found",
            Self::Conflict => "conflict",
            Self::Recovery(_) => "recovery_failed",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpApps {
    #[serde(default)]
    pub claude: bool,
    #[serde(default)]
    pub codex: bool,
    #[serde(default)]
    pub gemini: bool,
    #[serde(default)]
    pub grokbuild: bool,
    #[serde(default)]
    pub opencode: bool,
    #[serde(default)]
    pub hermes: bool,
}

impl McpApps {
    fn enabled(&self, app: &AppType) -> bool {
        match app {
            AppType::Claude => self.claude,
            AppType::Codex => self.codex,
            AppType::Gemini => self.gemini,
            AppType::GrokBuild => self.grokbuild,
            AppType::OpenCode => self.opencode,
            AppType::Hermes => self.hermes,
            AppType::ClaudeDesktop | AppType::OpenClaw | AppType::Pi => false,
        }
    }

    fn set(&mut self, app: &AppType, enabled: bool) -> Result<(), McpError> {
        match app {
            AppType::Claude => self.claude = enabled,
            AppType::Codex => self.codex = enabled,
            AppType::Gemini => self.gemini = enabled,
            AppType::GrokBuild => self.grokbuild = enabled,
            AppType::OpenCode => self.opencode = enabled,
            AppType::Hermes => self.hermes = enabled,
            AppType::ClaudeDesktop | AppType::OpenClaw | AppType::Pi => {
                return Err(McpError::InvalidServer(format!(
                    "application '{}' does not support MCP",
                    app.as_str()
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
        server: Value,
    },
    Remove {
        app: AppType,
        id: String,
    },
}

impl McpLiveChange {
    pub fn app(&self) -> &AppType {
        match self {
            Self::Upsert { app, .. } | Self::Remove { app, .. } => app,
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
    path: PathBuf,
    revision_key: [u8; 16],
}

impl McpStore {
    pub fn open(path: PathBuf) -> Result<Self, McpError> {
        let store = Self {
            path,
            revision_key: *Uuid::new_v4().as_bytes(),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn list(&self) -> Result<Vec<McpServer>, McpError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(&format!("{MCP_SELECT} ORDER BY name, id"))?;
        let rows = statement.query_map([], row_to_server)?;
        rows.map(|row| {
            let mut server = row.map_err(McpError::from)?;
            server.revision = server_revision(&server, &self.revision_key)?;
            Ok(server)
        })
        .collect()
    }

    pub fn upsert_with_live<T, E>(
        &self,
        server: McpServer,
        apply: impl FnOnce(&[McpLiveChange]) -> Result<T, E>,
        rollback: impl FnOnce(T) -> Result<(), String>,
    ) -> Result<Result<(), E>, McpError> {
        validate_server(&server)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_server(&transaction, &server.id, &self.revision_key)?;
        ensure_expected_revision(current.as_ref(), server.revision)?;
        let changes = live_changes(current.as_ref(), Some(&server));
        let receipt = match apply(&changes) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let finalize = (|| -> Result<(), McpError> {
            save_server(&transaction, &server)?;
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
        apply: impl FnOnce(&[McpLiveChange]) -> Result<T, E>,
        rollback: impl FnOnce(T) -> Result<(), String>,
    ) -> Result<Result<(), E>, McpError> {
        require_mcp_app(&app)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_server(&transaction, id, &self.revision_key)?
            .ok_or_else(|| McpError::NotFound(id.into()))?;
        ensure_expected_revision(Some(&current), expected_revision)?;
        if current.apps.enabled(&app) == enabled {
            transaction.commit()?;
            return Ok(Ok(()));
        }
        let mut updated = current.clone();
        updated.apps.set(&app, enabled)?;
        let changes = live_changes(Some(&current), Some(&updated));
        let receipt = match apply(&changes) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let finalize = (|| -> Result<(), McpError> {
            let column = enabled_column(&app)?;
            let changed = transaction.execute(
                &format!("UPDATE mcp_servers SET {column} = ?1 WHERE id = ?2"),
                params![enabled, id],
            )?;
            if changed != 1 {
                return Err(McpError::NotFound(id.to_owned()));
            }
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
        apply: impl FnOnce(&[McpLiveChange]) -> Result<T, E>,
        rollback: impl FnOnce(T) -> Result<(), String>,
    ) -> Result<Result<(), E>, McpError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_server(&transaction, id, &self.revision_key)?
            .ok_or_else(|| McpError::NotFound(id.into()))?;
        ensure_expected_revision(Some(&current), expected_revision)?;
        let changes = live_changes(Some(&current), None);
        let receipt = match apply(&changes) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let finalize = (|| -> Result<(), McpError> {
            let changed = transaction.execute("DELETE FROM mcp_servers WHERE id = ?1", [id])?;
            if changed != 1 {
                return Err(McpError::NotFound(id.to_owned()));
            }
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
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (imports, observation) = match observe() {
            Ok(observed) => observed,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        let report = match merge_imports(&transaction, imports, &self.revision_key) {
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
        let connection = self.connect()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS mcp_servers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                server_config TEXT NOT NULL,
                description TEXT,
                homepage TEXT,
                docs TEXT,
                tags TEXT NOT NULL DEFAULT '[]',
                enabled_claude BOOLEAN NOT NULL DEFAULT 0,
                enabled_codex BOOLEAN NOT NULL DEFAULT 0,
                enabled_gemini BOOLEAN NOT NULL DEFAULT 0,
                enabled_grokbuild BOOLEAN NOT NULL DEFAULT 0,
                enabled_opencode BOOLEAN NOT NULL DEFAULT 0,
                enabled_hermes BOOLEAN NOT NULL DEFAULT 0
            );",
        )?;
        for (column, definition) in [
            ("description", "TEXT"),
            ("homepage", "TEXT"),
            ("docs", "TEXT"),
            ("tags", "TEXT NOT NULL DEFAULT '[]'"),
            ("enabled_claude", "BOOLEAN NOT NULL DEFAULT 0"),
            ("enabled_codex", "BOOLEAN NOT NULL DEFAULT 0"),
            ("enabled_gemini", "BOOLEAN NOT NULL DEFAULT 0"),
            ("enabled_grokbuild", "BOOLEAN NOT NULL DEFAULT 0"),
            ("enabled_opencode", "BOOLEAN NOT NULL DEFAULT 0"),
            ("enabled_hermes", "BOOLEAN NOT NULL DEFAULT 0"),
        ] {
            ensure_column(&connection, column, definition)?;
        }
        verify_schema(&connection)
    }

    fn connect(&self) -> Result<Connection, McpError> {
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
}

fn merge_imports(
    transaction: &rusqlite::Transaction<'_>,
    imports: McpImportsByApp,
    revision_key: &[u8; 16],
) -> Result<McpImportReport, McpError> {
    let mut report = McpImportReport::default();
    for (app, imports) in imports {
        require_mcp_app(&app)?;
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
                if !mcp_servers_equivalent(&current.server, &import.server) {
                    report.failed_apps.push(format!(
                        "{}: server '{}' conflicts with the shared catalog",
                        app.as_str(),
                        import.id
                    ));
                    continue;
                }
                if current.apps.enabled(&app) != import.enabled {
                    current.apps.set(&app, import.enabled)?;
                    let column = enabled_column(&app)?;
                    transaction.execute(
                        &format!("UPDATE mcp_servers SET {column} = ?1 WHERE id = ?2"),
                        params![import.enabled, &import.id],
                    )?;
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
                    name: import.id,
                    server: import.server,
                    apps,
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: Vec::new(),
                    revision: 0,
                };
                validate_server(&server)?;
                save_server(transaction, &server)?;
                report.new_servers += 1;
            }
        }
    }
    Ok(report)
}

fn row_to_server(row: &Row<'_>) -> rusqlite::Result<McpServer> {
    let raw_server: String = row.get(2)?;
    let raw_tags: String = row.get(6)?;
    let server = serde_json::from_str(&raw_server).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let tags = serde_json::from_str(&raw_tags).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(McpServer {
        id: row.get(0)?,
        name: row.get(1)?,
        server,
        description: row.get(3)?,
        homepage: row.get(4)?,
        docs: row.get(5)?,
        tags,
        revision: 0,
        apps: McpApps {
            claude: row.get(7)?,
            codex: row.get(8)?,
            gemini: row.get(9)?,
            grokbuild: row.get(10)?,
            opencode: row.get(11)?,
            hermes: row.get(12)?,
        },
    })
}

fn get_server(
    connection: &Connection,
    id: &str,
    revision_key: &[u8; 16],
) -> Result<Option<McpServer>, McpError> {
    let mut server = connection
        .query_row(&format!("{MCP_SELECT} WHERE id = ?1"), [id], row_to_server)
        .optional()
        .map_err(McpError::from)?;
    if let Some(server) = &mut server {
        server.revision = server_revision(server, revision_key)?;
    }
    Ok(server)
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

fn server_revision(server: &McpServer, key: &[u8; 16]) -> Result<u64, McpError> {
    let snapshot = serde_json::to_vec(&(
        &server.id,
        &server.name,
        &server.server,
        &server.apps,
        &server.description,
        &server.homepage,
        &server.docs,
        &server.tags,
    ))
    .map_err(|error| McpError::InvalidStore(error.to_string()))?;
    let mut hasher = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 16-byte key");
    hasher.update(&snapshot);
    let bytes = hasher.finalize().into_bytes();
    let revision = u64::from_be_bytes(bytes[..8].try_into().expect("SHA-256 has eight bytes"));
    Ok((revision & SAFE_JS_INTEGER_MASK).max(1))
}

fn save_server(connection: &Connection, server: &McpServer) -> Result<(), McpError> {
    connection.execute(
        "INSERT INTO mcp_servers (
            id, name, server_config, description, homepage, docs, tags,
            enabled_claude, enabled_codex, enabled_gemini, enabled_grokbuild,
            enabled_opencode, enabled_hermes
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET
            name=excluded.name, server_config=excluded.server_config,
            description=excluded.description, homepage=excluded.homepage,
            docs=excluded.docs, tags=excluded.tags,
            enabled_claude=excluded.enabled_claude,
            enabled_codex=excluded.enabled_codex,
            enabled_gemini=excluded.enabled_gemini,
            enabled_grokbuild=excluded.enabled_grokbuild,
            enabled_opencode=excluded.enabled_opencode,
            enabled_hermes=excluded.enabled_hermes",
        params![
            server.id,
            server.name,
            serde_json::to_string(&server.server)
                .map_err(|error| McpError::InvalidServer(error.to_string()))?,
            server.description,
            server.homepage,
            server.docs,
            serde_json::to_string(&server.tags)
                .map_err(|error| McpError::InvalidServer(error.to_string()))?,
            server.apps.claude,
            server.apps.codex,
            server.apps.gemini,
            server.apps.grokbuild,
            server.apps.opencode,
            server.apps.hermes,
        ],
    )?;
    Ok(())
}

fn validate_server(server: &McpServer) -> Result<(), McpError> {
    validate_mcp_server(&server.id, &server.server)
        .map_err(|error| McpError::InvalidServer(error.to_string()))?;
    if server.name.trim().is_empty() || server.name.len() > 128 {
        return Err(McpError::InvalidServer(
            "name must contain at most 128 bytes".to_owned(),
        ));
    }
    for app in AppType::all() {
        if server.apps.enabled(&app) {
            require_mcp_app(&app)?;
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

fn require_mcp_app(app: &AppType) -> Result<(), McpError> {
    let descriptor = builtin_app_registry().for_app(app);
    let adapter = builtin_app_adapter(app);
    if descriptor.supports(AppCapability::Mcp) && adapter.mcp_config_target().is_some() {
        Ok(())
    } else {
        Err(McpError::InvalidServer(format!(
            "application '{}' does not support MCP",
            app.as_str()
        )))
    }
}

fn enabled_column(app: &AppType) -> Result<&'static str, McpError> {
    require_mcp_app(app)?;
    Ok(match app {
        AppType::Claude => "enabled_claude",
        AppType::Codex => "enabled_codex",
        AppType::Gemini => "enabled_gemini",
        AppType::GrokBuild => "enabled_grokbuild",
        AppType::OpenCode => "enabled_opencode",
        AppType::Hermes => "enabled_hermes",
        AppType::ClaudeDesktop | AppType::OpenClaw | AppType::Pi => unreachable!(),
    })
}

fn live_changes(before: Option<&McpServer>, after: Option<&McpServer>) -> Vec<McpLiveChange> {
    AppType::all()
        .filter(|app| {
            builtin_app_registry()
                .for_app(app)
                .supports(AppCapability::Mcp)
        })
        .filter_map(|app| {
            let was_enabled = before.is_some_and(|server| server.apps.enabled(&app));
            let is_enabled = after.is_some_and(|server| server.apps.enabled(&app));
            match (was_enabled, is_enabled, after) {
                (_, true, Some(server))
                    if !was_enabled
                        || before.is_none_or(|current| current.server != server.server) =>
                {
                    Some(McpLiveChange::Upsert {
                        app,
                        id: server.id.clone(),
                        server: server.server.clone(),
                    })
                }
                (true, false, _) => Some(McpLiveChange::Remove {
                    app,
                    id: before.expect("enabled prior server").id.clone(),
                }),
                _ => None,
            }
        })
        .collect()
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

fn ensure_column(connection: &Connection, column: &str, definition: &str) -> Result<(), McpError> {
    let mut statement = connection.prepare("PRAGMA table_info(mcp_servers)")?;
    let exists = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?
        .contains(column);
    if !exists {
        connection.execute_batch(&format!(
            "ALTER TABLE mcp_servers ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

fn verify_schema(connection: &Connection) -> Result<(), McpError> {
    let mut statement = connection.prepare("PRAGMA table_info(mcp_servers)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    for required in [
        "id",
        "name",
        "server_config",
        "description",
        "homepage",
        "docs",
        "tags",
        "enabled_claude",
        "enabled_codex",
        "enabled_gemini",
        "enabled_grokbuild",
        "enabled_opencode",
        "enabled_hermes",
    ] {
        if !columns.contains(required) {
            return Err(McpError::InvalidStore(format!(
                "mcp_servers is missing required column '{required}'"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn server() -> McpServer {
        McpServer {
            id: "context7".to_owned(),
            name: "Context7".to_owned(),
            server: json!({"type":"stdio","command":"npx","future":true}),
            apps: McpApps {
                claude: true,
                ..Default::default()
            },
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
        assert!(current.apps.codex);
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
        assert!(!store.list().unwrap().remove(0).apps.codex);
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
                    }]),
                ),
                (
                    AppType::Codex,
                    Ok(vec![cc_switch_core::McpImport {
                        id: "shared".to_owned(),
                        server: json!({"type":"stdio","command":"uvx"}),
                        enabled: true,
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
        assert!(current.apps.claude);
        assert!(!current.apps.codex);
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
                }]),
            )]
        };
        import_observed(&store, import(false), true).unwrap();
        assert!(!store.list().unwrap().remove(0).apps.opencode);

        let report = import_observed(&store, import(true), true).unwrap();
        assert_eq!(report.enabled_apps, 1);
        assert!(store.list().unwrap().remove(0).apps.opencode);
    }

    #[test]
    fn core_capabilities_and_database_flags_stay_aligned() {
        let ids = AppType::all()
            .filter(|app| {
                builtin_app_registry()
                    .for_app(app)
                    .supports(AppCapability::Mcp)
            })
            .map(|app| enabled_column(&app).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "enabled_claude",
                "enabled_codex",
                "enabled_gemini",
                "enabled_grokbuild",
                "enabled_opencode",
                "enabled_hermes"
            ]
        );
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
            }]),
        )];

        let error = import_observed(&store, imports, false).expect_err("changed live snapshot");

        assert!(matches!(error, McpError::Conflict));
        assert!(store.list().unwrap().is_empty());
    }
}
