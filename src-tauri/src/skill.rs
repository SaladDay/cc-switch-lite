use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Display,
    path::PathBuf,
    time::Duration,
};

use cc_switch_core::{builtin_app_registry, skill_directory_key, AppType, SkillActivationSource};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior};
use serde::Serialize;
use thiserror::Error;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SKILL_SELECT_BASE: &str = "SELECT id, name, description, directory, repo_owner, repo_name";

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("shared Skill database failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("shared Skill data is invalid: {0}")]
    InvalidStore(String),
    #[error("Skill is invalid: {0}")]
    InvalidSkill(String),
    #[error("Skill '{0}' was not found")]
    NotFound(String),
    #[error("shared Skill update failed and recovery was incomplete: {0}")]
    Recovery(String),
}

impl SkillError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_) => "storage_error",
            Self::InvalidStore(_) => "invalid_store",
            Self::InvalidSkill(_) => "invalid_skill",
            Self::NotFound(_) => "not_found",
            Self::Recovery(_) => "recovery_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRecord {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    pub apps: BTreeMap<String, bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub app_issues: BTreeMap<String, String>,
}

pub(crate) trait RecoverableSkillChange: Sized {
    type Error: Display;

    fn verify(&self) -> Result<(), Self::Error>;
    fn commit(self) -> Result<(), Self::Error>;
    fn rollback(self) -> Result<(), Self::Error>;
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

    pub fn list(&self) -> Result<Vec<SkillRecord>, SkillError> {
        let connection = self.connect()?;
        let bindings = catalog_bindings()?;
        let columns = bindings
            .iter()
            .map(|binding| binding.column)
            .collect::<Vec<_>>()
            .join(", ");
        let query = if columns.is_empty() {
            format!("{SKILL_SELECT_BASE} FROM skills ORDER BY name, id")
        } else {
            format!("{SKILL_SELECT_BASE}, {columns} FROM skills ORDER BY name, id")
        };
        let mut statement = connection.prepare(&query)?;
        let mut skills = statement
            .query_map([], |row| row_to_skill(row, &bindings))?
            .collect::<Result<Vec<_>, _>>()?;

        let mut directory_counts = HashMap::new();
        for skill in &skills {
            if let Ok(key) = skill_directory_key(&skill.directory) {
                *directory_counts.entry(key).or_insert(0) += 1;
            }
        }
        for skill in &mut skills {
            skill.issue = validate_catalog_entry(skill).err().or_else(|| {
                skill_directory_key(&skill.directory).ok().and_then(|key| {
                    (directory_counts.get(&key).copied().unwrap_or_default() > 1).then(|| {
                        format!(
                            "Multiple catalog entries use the '{}' directory",
                            skill.directory
                        )
                    })
                })
            });
        }
        Ok(skills)
    }

    pub fn toggle_with_live<C>(
        &self,
        id: &str,
        app: AppType,
        enabled: bool,
        apply: impl FnOnce(&str, &AppType, bool) -> Result<C, C::Error>,
    ) -> Result<Result<(), C::Error>, SkillError>
    where
        C: RecoverableSkillChange,
    {
        let descriptor = builtin_app_registry().for_app(&app);
        let contract = descriptor.skill_contract().ok_or_else(|| {
            SkillError::InvalidSkill(format!(
                "application '{}' does not support Skills",
                app.as_str()
            ))
        })?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let column = contract.catalog_column();
        let directory = get_skill_directory(&transaction, id)?
            .ok_or_else(|| SkillError::NotFound(id.to_owned()))?;
        let directory_key = skill_directory_key(&directory)
            .map_err(|error| SkillError::InvalidSkill(error.to_string()))?;
        let count = count_directory_key(&transaction, &directory_key)?;
        if count != 1 {
            return Err(SkillError::InvalidSkill(format!(
                "directory '{directory}' is not unique in the shared catalog"
            )));
        }
        let receipt = match apply(&directory, &app, enabled) {
            Ok(receipt) => receipt,
            Err(error) => {
                transaction.rollback()?;
                return Ok(Err(error));
            }
        };
        if let Some(column) = column {
            let update = transaction.execute(
                &format!("UPDATE skills SET {column} = ?1 WHERE id = ?2"),
                params![enabled, id],
            );
            match update {
                Ok(1) => {}
                Ok(_) => {
                    let error = SkillError::NotFound(id.to_owned());
                    let database_rollback = transaction.rollback();
                    return recover_database_failure(error, database_rollback, receipt);
                }
                Err(error) => {
                    let error = SkillError::Database(error);
                    let database_rollback = transaction.rollback();
                    return recover_database_failure(error, database_rollback, receipt);
                }
            }
        }

        if let Err(error) = receipt.verify() {
            let database_rollback = transaction.rollback();
            return recover_live_failure(error, database_rollback, receipt);
        }

        if let Err(error) = transaction.execute_batch("COMMIT") {
            let database_rollback = transaction.rollback();
            return recover_database_failure(
                SkillError::Database(error),
                database_rollback,
                receipt,
            );
        }
        drop(transaction);
        match receipt.commit() {
            Ok(()) => Ok(Ok(())),
            Err(error) => Ok(Err(error)),
        }
    }

    fn initialize(&self) -> Result<(), SkillError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
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
                installed_at INTEGER NOT NULL DEFAULT 0,
                content_hash TEXT,
                updated_at INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        let mut columns = skill_columns(&transaction)?;
        verify_base_columns(&columns)?;
        for binding in catalog_bindings()? {
            if columns.insert(binding.column.to_owned()) {
                transaction.execute_batch(&format!(
                    "ALTER TABLE skills ADD COLUMN {} BOOLEAN NOT NULL DEFAULT 0",
                    binding.column
                ))?;
            }
        }
        verify_schema(&transaction)?;
        transaction.commit().map_err(Into::into)
    }

    fn connect(&self) -> Result<Connection, SkillError> {
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

struct CatalogBinding {
    app_id: &'static str,
    column: &'static str,
}

fn catalog_bindings() -> Result<Vec<CatalogBinding>, SkillError> {
    builtin_app_registry()
        .descriptors()
        .filter_map(|descriptor| {
            let contract = descriptor.skill_contract()?;
            (contract.activation_source() == SkillActivationSource::CatalogFlag)
                .then_some((descriptor.id(), contract.catalog_column()))
        })
        .map(|(app_id, column)| {
            column
                .map(|column| CatalogBinding { app_id, column })
                .ok_or_else(|| {
                    SkillError::InvalidStore(format!(
                        "catalog-backed application '{app_id}' has no Skill column"
                    ))
                })
        })
        .collect()
}

fn row_to_skill(row: &Row<'_>, bindings: &[CatalogBinding]) -> rusqlite::Result<SkillRecord> {
    let mut apps = builtin_app_registry()
        .descriptors()
        .filter(|descriptor| descriptor.skill_contract().is_some())
        .map(|descriptor| (descriptor.id().to_owned(), false))
        .collect::<BTreeMap<_, _>>();
    for (index, binding) in bindings.iter().enumerate() {
        apps.insert(binding.app_id.to_owned(), row.get(6 + index)?);
    }
    Ok(SkillRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        directory: row.get(3)?,
        repo_owner: row.get(4)?,
        repo_name: row.get(5)?,
        apps,
        issue: None,
        app_issues: BTreeMap::new(),
    })
}

fn validate_catalog_entry(skill: &SkillRecord) -> Result<(), String> {
    if skill.id.trim().is_empty() {
        return Err("Skill catalog entry has an empty id".to_owned());
    }
    skill_directory_key(&skill.directory)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn count_directory_key(connection: &Connection, expected: &str) -> Result<usize, SkillError> {
    let mut statement = connection.prepare("SELECT directory FROM skills")?;
    let directories = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut count = 0;
    for directory in directories {
        if skill_directory_key(&directory?).is_ok_and(|key| key == expected) {
            count += 1;
        }
    }
    Ok(count)
}

fn get_skill_directory(connection: &Connection, id: &str) -> Result<Option<String>, SkillError> {
    connection
        .query_row("SELECT directory FROM skills WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(Into::into)
}

fn recover_live_failure<C: RecoverableSkillChange>(
    error: C::Error,
    database_rollback: rusqlite::Result<()>,
    receipt: C,
) -> Result<Result<(), C::Error>, SkillError> {
    let live_rollback = receipt.rollback();
    match (database_rollback, live_rollback) {
        (Ok(()), Ok(())) => Ok(Err(error)),
        (database, live) => Err(SkillError::Recovery(format_recovery(
            "live verification",
            &error,
            database.err(),
            live.err(),
        ))),
    }
}

fn recover_database_failure<C: RecoverableSkillChange>(
    error: SkillError,
    database_rollback: rusqlite::Result<()>,
    receipt: C,
) -> Result<Result<(), C::Error>, SkillError> {
    let live_rollback = receipt.rollback();
    if database_rollback.is_ok() && live_rollback.is_ok() {
        return Err(error);
    }
    Err(SkillError::Recovery(format_recovery(
        "database update",
        &error,
        database_rollback.err(),
        live_rollback.err(),
    )))
}

fn format_recovery(
    context: &str,
    cause: &impl Display,
    database: Option<rusqlite::Error>,
    live: Option<impl Display>,
) -> String {
    let mut message = format!("{context} failed: {cause}");
    if let Some(error) = database {
        message.push_str(&format!("; database rollback: {error}"));
    }
    if let Some(error) = live {
        message.push_str(&format!("; live rollback: {error}"));
    }
    message
}

fn skill_columns(connection: &Connection) -> Result<HashSet<String>, SkillError> {
    let mut statement = connection.prepare("PRAGMA table_info(skills)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(columns)
}

fn verify_schema(connection: &Connection) -> Result<(), SkillError> {
    let columns = skill_columns(connection)?;
    verify_base_columns(&columns)?;
    for binding in catalog_bindings()? {
        if !columns.contains(binding.column) {
            return Err(SkillError::InvalidStore(format!(
                "skills is missing required column '{}'",
                binding.column
            )));
        }
    }
    Ok(())
}

fn verify_base_columns(columns: &HashSet<String>) -> Result<(), SkillError> {
    for required in [
        "id",
        "name",
        "description",
        "directory",
        "repo_owner",
        "repo_name",
    ] {
        if !columns.contains(required) {
            return Err(SkillError::InvalidStore(format!(
                "skills is missing required column '{required}'"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs, rc::Rc};

    use super::*;
    use crate::live::LiveConfig;
    use tempfile::tempdir;

    #[derive(Clone)]
    struct FakeReceipt {
        verified: bool,
        committed: Rc<Cell<bool>>,
        rolled_back: Rc<Cell<bool>>,
    }

    impl RecoverableSkillChange for FakeReceipt {
        type Error = &'static str;

        fn verify(&self) -> Result<(), Self::Error> {
            self.verified.then_some(()).ok_or("verification failed")
        }

        fn commit(self) -> Result<(), Self::Error> {
            self.committed.set(true);
            Ok(())
        }

        fn rollback(self) -> Result<(), Self::Error> {
            self.rolled_back.set(true);
            Ok(())
        }
    }

    fn seed_store() -> (tempfile::TempDir, PathBuf, SkillStore) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let store = SkillStore::open(path.clone()).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO skills (id, name, description, directory, repo_owner, repo_name)
                 VALUES ('docs', 'Docs', 'Documentation', 'docs', 'owner', 'repo')",
                [],
            )
            .unwrap();
        (directory, path, store)
    }

    #[test]
    fn incompatible_skill_schema_is_rejected_without_partial_migration() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE skills (directory TEXT NOT NULL);")
            .unwrap();

        assert!(matches!(
            SkillStore::open(path.clone()),
            Err(SkillError::InvalidStore(_))
        ));
        let connection = Connection::open(path).unwrap();
        let columns = skill_columns(&connection).unwrap();
        assert_eq!(columns, HashSet::from(["directory".to_owned()]));
    }

    #[test]
    fn shared_schema_lists_catalog_apps_and_preserves_future_columns() {
        let (_directory, path, store) = seed_store();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE skills ADD COLUMN future TEXT NOT NULL DEFAULT 'keep';
                 UPDATE skills SET enabled_claude = 1;",
            )
            .unwrap();

        let skills = store.list().unwrap();
        assert_eq!(skills.len(), 1);
        assert!(skills[0].apps["claude"]);
        assert!(!skills[0].apps["pi"]);
        assert_eq!(skills[0].issue, None);
        assert_eq!(
            connection
                .query_row("SELECT future FROM skills", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "keep"
        );
    }

    #[test]
    fn toggle_uses_the_core_catalog_binding_only() {
        let (_directory, path, store) = seed_store();
        let committed = Rc::new(Cell::new(false));
        let rolled_back = Rc::new(Cell::new(false));
        store
            .toggle_with_live("docs", AppType::Claude, true, |directory, app, enabled| {
                assert_eq!(directory, "docs");
                assert_eq!(app, &AppType::Claude);
                assert!(enabled);
                Ok(FakeReceipt {
                    verified: true,
                    committed: committed.clone(),
                    rolled_back: rolled_back.clone(),
                })
            })
            .unwrap()
            .unwrap();

        assert!(committed.get());
        assert!(!rolled_back.get());
        assert!(Connection::open(path)
            .unwrap()
            .query_row("SELECT enabled_claude FROM skills", [], |row| row
                .get::<_, bool>(0))
            .unwrap());
    }

    #[test]
    fn failed_live_verification_rolls_back_the_catalog() {
        let (_directory, path, store) = seed_store();
        let rolled_back = Rc::new(Cell::new(false));
        let result = store
            .toggle_with_live("docs", AppType::Codex, true, |_, _, _| {
                Ok(FakeReceipt {
                    verified: false,
                    committed: Rc::new(Cell::new(false)),
                    rolled_back: rolled_back.clone(),
                })
            })
            .unwrap();

        assert_eq!(result, Err("verification failed"));
        assert!(rolled_back.get());
        assert!(!Connection::open(path)
            .unwrap()
            .query_row("SELECT enabled_codex FROM skills", [], |row| row
                .get::<_, bool>(0))
            .unwrap());
    }

    #[test]
    fn duplicate_or_unsafe_directories_never_reach_live_changes() {
        let (_directory, path, store) = seed_store();
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "INSERT INTO skills (id, name, directory) VALUES ('duplicate', 'Duplicate', 'docs')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO skills (id, name, directory) VALUES ('unsafe', 'Unsafe', '../unsafe')",
                [],
            )
            .unwrap();
        let skills = store.list().unwrap();
        assert!(skills.iter().all(|skill| skill.issue.is_some()));

        for id in ["docs", "unsafe"] {
            let result =
                store.toggle_with_live::<FakeReceipt>(id, AppType::Claude, true, |_, _, _| {
                    panic!("invalid catalog rows must not reach live changes")
                });
            assert!(matches!(result, Err(SkillError::InvalidSkill(_))));
        }
    }

    #[test]
    fn portable_directory_aliases_are_read_only() {
        let (_directory, path, store) = seed_store();
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "INSERT INTO skills (id, name, directory) VALUES ('accent-a', 'A', 'É')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO skills (id, name, directory) VALUES ('accent-b', 'B', 'é')",
                [],
            )
            .unwrap();

        let skills = store.list().unwrap();
        assert!(skills
            .iter()
            .filter(|skill| skill.id.starts_with("accent-"))
            .all(|skill| skill.issue.is_some()));
        assert!(matches!(
            store.toggle_with_live::<FakeReceipt>(
                "accent-a",
                AppType::Claude,
                true,
                |_, _, _| panic!("aliased catalog rows must not reach live changes")
            ),
            Err(SkillError::InvalidSkill(_))
        ));
    }

    #[test]
    fn shared_catalog_and_native_skill_change_commit_together() {
        let directory = tempdir().unwrap();
        let shared = directory.path().join(".cc-switch");
        let source = shared.join("skills/docs");
        let claude = directory.path().join("claude");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&claude).unwrap();
        fs::write(source.join("SKILL.md"), "# Docs").unwrap();
        fs::write(
            shared.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "claudeConfigDir": claude,
                "skillSyncMethod": "copy"
            }))
            .unwrap(),
        )
        .unwrap();
        let path = shared.join("cc-switch.db");
        let store = SkillStore::open(path.clone()).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO skills (id, name, directory) VALUES ('docs', 'Docs', 'docs')",
                [],
            )
            .unwrap();
        let live = LiveConfig::from_home(directory.path()).unwrap();

        let residual = claude.join("skills/docs");
        fs::create_dir_all(&residual).unwrap();
        fs::write(residual.join("SKILL.md"), "# Docs").unwrap();
        store
            .toggle_with_live("docs", AppType::Claude, false, |directory, app, enabled| {
                live.apply_skill_recoverable(directory, app, enabled)
            })
            .unwrap()
            .unwrap();
        assert!(!residual.exists());

        for enabled in [true, false] {
            store
                .toggle_with_live(
                    "docs",
                    AppType::Claude,
                    enabled,
                    |directory, app, enabled| live.apply_skill_recoverable(directory, app, enabled),
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                Connection::open(&path)
                    .unwrap()
                    .query_row("SELECT enabled_claude FROM skills", [], |row| {
                        row.get::<_, bool>(0)
                    })
                    .unwrap(),
                enabled
            );
            assert_eq!(claude.join("skills/docs").exists(), enabled);
        }
        assert!(source.join("SKILL.md").exists());
    }
}
