use std::{collections::HashSet, fs, path::PathBuf, time::Duration};

use cc_switch_core::{
    skill_catalog_columns, AppType, InstalledSkillSnapshot, SkillCatalogDecision,
    SkillCatalogEntry, SkillCatalogEntryError, SkillControlReason,
};
use rusqlite::{params, Connection, OpenFlags, Row, TransactionBehavior};
use thiserror::Error;

use crate::{
    live::{LiveConfig, SkillWriteReceipt},
    skill_live::SkillHostError,
};

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("shared Skill database failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("shared Skill data is invalid: {0}")]
    InvalidStore(String),
    #[error("Skill changed outside this editor; reload and try again")]
    Conflict,
    #[error(transparent)]
    Host(#[from] SkillHostError),
    #[error("shared Skill update failed and live recovery was incomplete: {0}")]
    Recovery(String),
    #[error("shared Skill database I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl SkillError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_) | Self::Io { .. } => "storage_error",
            Self::InvalidStore(_) => "invalid_store",
            Self::Conflict => "conflict",
            Self::Host(error) => error.code(),
            Self::Recovery(_) => "recovery_failed",
        }
    }
}

pub struct SkillStore {
    path: PathBuf,
}

impl SkillStore {
    pub fn open(path: PathBuf) -> Result<Self, SkillError> {
        let store = Self { path };
        store.initialize()?;
        Ok(store)
    }

    pub fn list(&self, live: &LiveConfig) -> Result<Vec<InstalledSkillSnapshot>, SkillError> {
        let catalog = load_catalog(&self.connect()?)?;
        live.inspect_skills(&catalog).map_err(Into::into)
    }

    pub fn toggle(
        &self,
        live: &LiveConfig,
        skill_id: &str,
        app: AppType,
        enabled: bool,
    ) -> Result<(), SkillError> {
        if cc_switch_core::builtin_app_registry()
            .for_app(&app)
            .skill_contract()
            .is_none()
        {
            return Err(SkillHostError::UnsupportedApp(app.as_str().to_owned()).into());
        }

        self.change(live, skill_id, app, Some(enabled))
    }

    pub fn reconcile_pending(&self, live: &LiveConfig) -> Result<Vec<String>, SkillError> {
        let pending = self
            .list(live)?
            .into_iter()
            .flat_map(|skill| {
                let skill_id = skill.id().to_owned();
                skill
                    .apps()
                    .filter_map(move |state| {
                        matches!(
                            state.reason(),
                            Some(
                                SkillControlReason::RecoveryPending
                                    | SkillControlReason::ManagedReferenceDrift
                                    | SkillControlReason::CatalogDrift
                            )
                        )
                        .then(|| (skill_id.clone(), state.app().clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let failures = pending
            .into_iter()
            .filter_map(|(skill_id, app)| {
                self.change(live, &skill_id, app.clone(), None)
                    .err()
                    .map(|error| format!("{skill_id}/{}: {error}", app.as_str()))
            })
            .collect();
        Ok(failures)
    }

    fn change(
        &self,
        live: &LiveConfig,
        skill_id: &str,
        app: AppType,
        requested: Option<bool>,
    ) -> Result<(), SkillError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let catalog = load_catalog(&transaction)?;
        let receipt = live.apply_skill_recoverable(&catalog, skill_id, &app, requested)?;

        let database_result = (|| -> Result<(), SkillError> {
            apply_catalog_change(&transaction, &catalog, &receipt)?;
            transaction.commit()?;
            Ok(())
        })();

        match database_result {
            Ok(()) => commit_live(live, receipt),
            Err(SkillError::Database(database_error)) => {
                self.resolve_uncertain_commit(live, receipt, skill_id, database_error)
            }
            Err(error) => rollback_live(live, receipt, error),
        }
    }

    fn resolve_uncertain_commit(
        &self,
        live: &LiveConfig,
        receipt: SkillWriteReceipt<'_>,
        skill_id: &str,
        database_error: rusqlite::Error,
    ) -> Result<(), SkillError> {
        let decision = self
            .connect()
            .and_then(|connection| load_catalog(&connection))
            .map(|catalog| {
                receipt
                    .value
                    .decide_catalog(catalog.iter().find(|entry| entry.id() == skill_id))
            });
        match decision {
            Ok(SkillCatalogDecision::KeepLive) => commit_live(live, receipt),
            Ok(SkillCatalogDecision::RestoreLive) => {
                rollback_live(live, receipt, SkillError::Database(database_error))
            }
            Ok(SkillCatalogDecision::Conflict) => {
                rollback_live(live, receipt, SkillError::Conflict)
            }
            Err(read_error) => rollback_live(
                live,
                receipt,
                SkillError::Recovery(format!(
                    "database commit error: {database_error}; catalog re-read error: {read_error}"
                )),
            ),
        }
    }

    fn initialize(&self) -> Result<(), SkillError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| SkillError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        let connection = self.connect()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS skills (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                directory TEXT NOT NULL,
                repo_owner TEXT,
                repo_name TEXT,
                repo_branch TEXT DEFAULT 'main',
                readme_url TEXT,
                enabled_claude BOOLEAN NOT NULL DEFAULT 0,
                enabled_codex BOOLEAN NOT NULL DEFAULT 0,
                enabled_gemini BOOLEAN NOT NULL DEFAULT 0,
                enabled_grokbuild BOOLEAN NOT NULL DEFAULT 0,
                enabled_opencode BOOLEAN NOT NULL DEFAULT 0,
                enabled_hermes BOOLEAN NOT NULL DEFAULT 0,
                enabled_pi BOOLEAN NOT NULL DEFAULT 0,
                installed_at INTEGER NOT NULL DEFAULT 0,
                content_hash TEXT,
                updated_at INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        for column in skill_catalog_columns() {
            ensure_column(&connection, column.as_str())?;
        }
        verify_schema(&connection)
    }

    fn connect(&self) -> Result<Connection, SkillError> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        Ok(connection)
    }
}

fn apply_catalog_change(
    transaction: &rusqlite::Transaction<'_>,
    catalog: &[SkillCatalogEntry],
    receipt: &SkillWriteReceipt<'_>,
) -> Result<(), SkillError> {
    let guard = receipt.value.plan().catalog_guard();
    let current = catalog
        .iter()
        .find(|entry| entry.id() == guard.skill_id())
        .ok_or(SkillError::Conflict)?;
    if !guard.matches(current) {
        return Err(SkillError::Conflict);
    }
    let Some(change) = receipt.value.plan().catalog_change() else {
        return Ok(());
    };
    let column = change.column().as_str();
    let changed = transaction.execute(
        &format!(
            "UPDATE skills SET \"{column}\" = ?1
             WHERE id = ?2 AND name = ?3 AND directory = ?4 AND \"{column}\" = ?5"
        ),
        params![
            change.replacement(),
            change.skill_id(),
            guard.expected_name(),
            guard.expected_directory(),
            change.expected(),
        ],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(SkillError::Conflict)
    }
}

fn commit_live(live: &LiveConfig, receipt: SkillWriteReceipt<'_>) -> Result<(), SkillError> {
    live.commit_skill(receipt).map_err(|error| {
        SkillError::Recovery(format!(
            "catalog was committed but live verification failed: {error}"
        ))
    })
}

fn rollback_live(
    live: &LiveConfig,
    receipt: SkillWriteReceipt<'_>,
    error: SkillError,
) -> Result<(), SkillError> {
    match live.rollback_skill(receipt) {
        Ok(()) => Err(error),
        Err(rollback_error) => Err(SkillError::Recovery(format!(
            "update error: {error}; live recovery error: {rollback_error}"
        ))),
    }
}

fn load_catalog(connection: &Connection) -> Result<Vec<SkillCatalogEntry>, SkillError> {
    let columns = skill_catalog_columns().collect::<Vec<_>>();
    let selection_columns = columns
        .iter()
        .map(|column| format!("\"{}\"", column.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT id, name, description, directory, {selection_columns}
         FROM skills ORDER BY name, id"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| raw_skill(row, columns.len()))?;
    rows.map(|row| {
        let row = row?;
        SkillCatalogEntry::try_new(
            row.id,
            row.name,
            row.description,
            row.directory,
            columns.iter().copied().zip(row.selections),
        )
        .map_err(invalid_catalog_entry)
    })
    .collect()
}

struct RawSkill {
    id: String,
    name: String,
    description: Option<String>,
    directory: String,
    selections: Vec<bool>,
}

fn raw_skill(row: &Row<'_>, selection_count: usize) -> rusqlite::Result<RawSkill> {
    let selections = (0..selection_count)
        .map(|offset| row.get(4 + offset))
        .collect::<Result<Vec<bool>, _>>()?;
    Ok(RawSkill {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        directory: row.get(3)?,
        selections,
    })
}

fn invalid_catalog_entry(error: SkillCatalogEntryError) -> SkillError {
    SkillError::InvalidStore(error.to_string())
}

fn ensure_column(connection: &Connection, column: &str) -> Result<(), SkillError> {
    let columns = table_columns(connection)?;
    if !columns.contains(column) {
        connection.execute_batch(&format!(
            "ALTER TABLE skills ADD COLUMN \"{column}\" BOOLEAN NOT NULL DEFAULT 0"
        ))?;
    }
    Ok(())
}

fn verify_schema(connection: &Connection) -> Result<(), SkillError> {
    let columns = table_columns(connection)?;
    for required in ["id", "name", "description", "directory"]
        .into_iter()
        .chain(skill_catalog_columns().map(|column| column.as_str()))
    {
        if !columns.contains(required) {
            return Err(SkillError::InvalidStore(format!(
                "skills table is missing required column '{required}'"
            )));
        }
    }
    Ok(())
}

fn table_columns(connection: &Connection) -> Result<HashSet<String>, SkillError> {
    let mut statement = connection.prepare("PRAGMA table_info(skills)")?;
    let columns = statement
        .query_map([], |row| row.get(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn insert_skill(path: &Path) {
        Connection::open(path)
            .unwrap()
            .execute(
                "INSERT INTO skills (id, name, description, directory)
                 VALUES ('demo', 'Demo', 'Demo Skill', 'demo')",
                [],
            )
            .unwrap();
    }

    #[test]
    fn shared_schema_uses_every_core_selection_column() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = SkillStore::open(path.clone()).unwrap();
        insert_skill(&path);

        let catalog = load_catalog(&store.connect().unwrap()).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(
            catalog[0]
                .selections()
                .map(|(column, selected)| (column.as_str(), selected))
                .collect::<Vec<_>>(),
            skill_catalog_columns()
                .map(|column| (column.as_str(), false))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn toggle_commits_the_shared_catalog_and_core_live_state() {
        let home = tempdir().unwrap();
        let path = home.path().join(".cc-switch/cc-switch.db");
        let store = SkillStore::open(path.clone()).unwrap();
        insert_skill(&path);
        let source = home.path().join(".cc-switch/skills/demo");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "# Demo\n").unwrap();
        let live = LiveConfig::from_home(home.path()).unwrap();

        store.toggle(&live, "demo", AppType::Claude, true).unwrap();
        let snapshots = store.list(&live).unwrap();
        let claude = snapshots[0]
            .apps()
            .find(|state| state.app() == &AppType::Claude)
            .unwrap();

        assert_eq!(claude.selected(), Some(true));
        assert_eq!(claude.enabled(), Some(true));
        assert!(home.path().join(".claude/skills/demo").exists());
    }

    #[test]
    fn startup_reconciliation_repairs_managed_reference_drift() {
        let home = tempdir().unwrap();
        let path = home.path().join(".cc-switch/cc-switch.db");
        let store = SkillStore::open(path.clone()).unwrap();
        insert_skill(&path);
        let source = home.path().join(".cc-switch/skills/demo");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "# Demo\n").unwrap();
        let live = LiveConfig::from_home(home.path()).unwrap();

        store.toggle(&live, "demo", AppType::Claude, true).unwrap();
        fs::remove_file(home.path().join(".claude/skills/demo")).unwrap();

        assert!(store.reconcile_pending(&live).unwrap().is_empty());
        let snapshots = store.list(&live).unwrap();
        let claude = snapshots[0]
            .apps()
            .find(|state| state.app() == &AppType::Claude)
            .unwrap();
        assert_eq!(claude.selected(), Some(true));
        assert_eq!(claude.enabled(), Some(true));
        assert_eq!(claude.reason(), None);
    }
}
