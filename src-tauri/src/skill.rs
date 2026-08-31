use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Display,
    path::PathBuf,
    time::Duration,
};

use cc_switch_core::{builtin_app_registry, skill_directory_key, AppType};
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
    pub apps: BTreeMap<String, SkillAppState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAppState {
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingSkillChange {
    pub(crate) skill_id: String,
    pub(crate) name: String,
    pub(crate) directory: String,
    pub(crate) app: AppType,
    pub(crate) enabled: bool,
    previous_enabled: Option<bool>,
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
        let bindings = catalog_bindings();
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
        apply: impl FnOnce(&PendingSkillChange) -> Result<C, C::Error>,
    ) -> Result<Result<(), C::Error>, SkillError>
    where
        C: RecoverableSkillChange,
    {
        let pending = self.begin_toggle(id, app, enabled)?;
        let receipt = match apply(&pending) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.cancel_pending(&pending)?;
                return Ok(Err(error));
            }
        };
        if let Err(error) = receipt.verify() {
            return match receipt.rollback() {
                Ok(()) => match self.cancel_pending(&pending) {
                    Ok(()) => Ok(Err(error)),
                    Err(database) => Err(SkillError::Recovery(format_recovery(
                        "live verification",
                        &error,
                        Some(database),
                        None::<C::Error>,
                    ))),
                },
                Err(live) => Err(SkillError::Recovery(format_recovery(
                    "live verification",
                    &error,
                    None,
                    Some(live),
                ))),
            };
        }
        if let Err(error) = receipt.commit() {
            return Ok(Err(error));
        }
        self.complete_pending(&pending)?;
        Ok(Ok(()))
    }

    pub(crate) fn recover_pending_with_live<C>(
        &self,
        mut apply: impl FnMut(&PendingSkillChange) -> Result<C, C::Error>,
    ) -> Result<Result<(), C::Error>, SkillError>
    where
        C: RecoverableSkillChange,
    {
        for pending in self.pending_changes()? {
            let receipt = match apply(&pending) {
                Ok(receipt) => receipt,
                Err(error) => return Ok(Err(error)),
            };
            if let Err(error) = receipt.verify() {
                return match receipt.rollback() {
                    Ok(()) => Ok(Err(error)),
                    Err(rollback) => Err(SkillError::Recovery(format_recovery(
                        "pending live verification",
                        &error,
                        None,
                        Some(rollback),
                    ))),
                };
            }
            if let Err(error) = receipt.commit() {
                return Ok(Err(error));
            }
            self.complete_pending(&pending)?;
        }
        Ok(Ok(()))
    }

    fn begin_toggle(
        &self,
        id: &str,
        app: AppType,
        enabled: bool,
    ) -> Result<PendingSkillChange, SkillError> {
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
        let query = column.map_or_else(
            || "SELECT name, directory, NULL FROM skills WHERE id = ?1".to_owned(),
            |column| format!("SELECT name, directory, {column} FROM skills WHERE id = ?1"),
        );
        let row = transaction
            .query_row(&query, [id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<bool>>(2)?,
                ))
            })
            .optional()?
            .ok_or_else(|| SkillError::NotFound(id.to_owned()))?;
        validate_unique_directory(&transaction, &row.1)?;
        let pending = PendingSkillChange {
            skill_id: id.to_owned(),
            name: row.0,
            directory: row.1,
            app,
            enabled,
            previous_enabled: row.2,
        };
        transaction
            .execute(
                "INSERT INTO skill_operation_journal
                 (skill_id, app_id, skill_name, directory, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    pending.skill_id,
                    pending.app.as_str(),
                    pending.name,
                    pending.directory,
                    pending.enabled
                ],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(ref failure, _)
                    if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    SkillError::Recovery(format!(
                        "a pending Skill change already exists for '{}' and '{}'",
                        pending.skill_id,
                        pending.app.as_str()
                    ))
                }
                error => SkillError::Database(error),
            })?;
        if let Some(column) = column {
            let updated = transaction.execute(
                &format!("UPDATE skills SET {column} = ?1 WHERE id = ?2"),
                params![enabled, id],
            )?;
            if updated != 1 {
                return Err(SkillError::NotFound(id.to_owned()));
            }
        }
        transaction.commit()?;
        Ok(pending)
    }

    fn pending_changes(&self) -> Result<Vec<PendingSkillChange>, SkillError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT skill_id, app_id, skill_name, directory, enabled
             FROM skill_operation_journal ORDER BY skill_id, app_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        let mut pending = Vec::new();
        for row in rows {
            let (skill_id, app_id, name, directory, enabled) = row?;
            let app = app_id.parse::<AppType>().map_err(|_| {
                SkillError::InvalidStore(format!(
                    "pending Skill change uses unknown application '{app_id}'"
                ))
            })?;
            let contract = builtin_app_registry()
                .for_app(&app)
                .skill_contract()
                .ok_or_else(|| {
                    SkillError::InvalidStore(format!(
                        "pending Skill change uses application '{app_id}' without Skills"
                    ))
                })?;
            let current = connection
                .query_row(
                    "SELECT name, directory FROM skills WHERE id = ?1",
                    [&skill_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
                .ok_or_else(|| {
                    SkillError::InvalidStore(format!(
                        "pending Skill change references missing Skill '{skill_id}'"
                    ))
                })?;
            if current != (name.clone(), directory.clone()) {
                return Err(SkillError::InvalidStore(format!(
                    "Skill '{skill_id}' changed while a pending operation was recorded"
                )));
            }
            validate_unique_directory(&connection, &directory)?;
            if let Some(column) = contract.catalog_column() {
                let desired = connection.query_row(
                    &format!("SELECT {column} FROM skills WHERE id = ?1"),
                    [&skill_id],
                    |row| row.get::<_, bool>(0),
                )?;
                if desired != enabled {
                    return Err(SkillError::InvalidStore(format!(
                        "pending Skill change for '{skill_id}' does not match the catalog"
                    )));
                }
            }
            pending.push(PendingSkillChange {
                skill_id,
                name,
                directory,
                app,
                enabled,
                previous_enabled: None,
            });
        }
        Ok(pending)
    }

    fn cancel_pending(&self, pending: &PendingSkillChange) -> Result<(), SkillError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(previous) = pending.previous_enabled {
            let column = builtin_app_registry()
                .for_app(&pending.app)
                .skill_contract()
                .and_then(|contract| contract.catalog_column())
                .ok_or_else(|| {
                    SkillError::Recovery(format!(
                        "missing catalog binding for '{}'",
                        pending.app.as_str()
                    ))
                })?;
            let updated = transaction.execute(
                &format!(
                    "UPDATE skills SET {column} = ?1
                     WHERE id = ?2 AND {column} = ?3"
                ),
                params![previous, pending.skill_id, pending.enabled],
            )?;
            if updated != 1 {
                return Err(SkillError::Recovery(format!(
                    "catalog changed while rolling back Skill '{}'",
                    pending.skill_id
                )));
            }
        }
        delete_pending(&transaction, pending)?;
        transaction.commit().map_err(Into::into)
    }

    fn complete_pending(&self, pending: &PendingSkillChange) -> Result<(), SkillError> {
        let connection = self.connect()?;
        delete_pending(&connection, pending)
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
                installed_at INTEGER NOT NULL DEFAULT 0,
                content_hash TEXT,
                updated_at INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS skill_operation_journal (
                skill_id TEXT NOT NULL,
                app_id TEXT NOT NULL,
                skill_name TEXT NOT NULL,
                directory TEXT NOT NULL,
                enabled BOOLEAN NOT NULL,
                PRIMARY KEY (skill_id, app_id)
            );",
        )?;
        let mut columns = skill_columns(&transaction)?;
        verify_base_columns(&columns)?;
        verify_journal_schema(&transaction)?;
        for binding in catalog_bindings() {
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

fn catalog_bindings() -> Vec<CatalogBinding> {
    builtin_app_registry()
        .descriptors()
        .filter_map(|descriptor| {
            let contract = descriptor.skill_contract()?;
            contract.catalog_column().map(|column| CatalogBinding {
                app_id: descriptor.id(),
                column,
            })
        })
        .collect()
}

fn row_to_skill(row: &Row<'_>, bindings: &[CatalogBinding]) -> rusqlite::Result<SkillRecord> {
    let mut apps = builtin_app_registry()
        .descriptors()
        .filter_map(|descriptor| Some((descriptor, descriptor.skill_contract()?)))
        .map(|(descriptor, contract)| {
            let enabled = contract.catalog_column().map(|_| false);
            (
                descriptor.id().to_owned(),
                SkillAppState {
                    enabled,
                    issue: None,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (index, binding) in bindings.iter().enumerate() {
        apps.insert(
            binding.app_id.to_owned(),
            SkillAppState {
                enabled: Some(row.get(6 + index)?),
                issue: None,
            },
        );
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

fn validate_unique_directory(connection: &Connection, directory: &str) -> Result<(), SkillError> {
    let directory_key = skill_directory_key(directory)
        .map_err(|error| SkillError::InvalidSkill(error.to_string()))?;
    if count_directory_key(connection, &directory_key)? != 1 {
        return Err(SkillError::InvalidSkill(format!(
            "directory '{directory}' is not unique in the shared catalog"
        )));
    }
    Ok(())
}

fn delete_pending(connection: &Connection, pending: &PendingSkillChange) -> Result<(), SkillError> {
    let deleted = connection.execute(
        "DELETE FROM skill_operation_journal
         WHERE skill_id = ?1 AND app_id = ?2 AND skill_name = ?3
           AND directory = ?4 AND enabled = ?5",
        params![
            pending.skill_id,
            pending.app.as_str(),
            pending.name,
            pending.directory,
            pending.enabled
        ],
    )?;
    if deleted != 1 {
        return Err(SkillError::Recovery(format!(
            "pending Skill change disappeared for '{}' and '{}'",
            pending.skill_id,
            pending.app.as_str()
        )));
    }
    Ok(())
}

fn format_recovery(
    context: &str,
    cause: &impl Display,
    database: Option<SkillError>,
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
    for binding in catalog_bindings() {
        if !columns.contains(binding.column) {
            return Err(SkillError::InvalidStore(format!(
                "skills is missing required column '{}'",
                binding.column
            )));
        }
    }
    Ok(())
}

fn verify_journal_schema(connection: &Connection) -> Result<(), SkillError> {
    let mut statement = connection.prepare("PRAGMA table_info(skill_operation_journal)")?;
    let info = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, usize>(5)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let columns = info
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<HashSet<_>>();
    for required in ["skill_id", "app_id", "skill_name", "directory", "enabled"] {
        if !columns.contains(required) {
            return Err(SkillError::InvalidStore(format!(
                "skill_operation_journal is missing required column '{required}'"
            )));
        }
    }
    let primary_key = info
        .iter()
        .filter(|(_, position)| *position > 0)
        .map(|(name, position)| (name.as_str(), *position))
        .collect::<Vec<_>>();
    if primary_key != [("skill_id", 1), ("app_id", 2)] {
        return Err(SkillError::InvalidStore(
            "skill_operation_journal has an incompatible primary key".to_owned(),
        ));
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
    fn incompatible_journal_key_is_rejected() {
        let (_directory, path, store) = seed_store();
        drop(store);
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "DROP TABLE skill_operation_journal;
                 CREATE TABLE skill_operation_journal (
                    skill_id TEXT NOT NULL,
                    app_id TEXT NOT NULL,
                    skill_name TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    enabled BOOLEAN NOT NULL,
                    generation INTEGER,
                    PRIMARY KEY (skill_id, app_id, generation)
                 );",
            )
            .unwrap();

        assert!(matches!(
            SkillStore::open(path),
            Err(SkillError::InvalidStore(_))
        ));
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
        assert_eq!(skills[0].apps["claude"].enabled, Some(true));
        assert_eq!(skills[0].apps["pi"].enabled, None);
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
            .toggle_with_live("docs", AppType::Claude, true, |pending| {
                assert_eq!(pending.directory, "docs");
                assert_eq!(pending.name, "Docs");
                assert_eq!(pending.app, AppType::Claude);
                assert!(pending.enabled);
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
    fn durable_intent_is_replayed_after_an_interrupted_toggle() {
        let (_directory, path, store) = seed_store();
        let pending = store.begin_toggle("docs", AppType::Claude, true).unwrap();
        assert!(pending.enabled);
        assert_eq!(
            Connection::open(&path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM skill_operation_journal", [], |row| {
                    row.get::<_, usize>(0)
                })
                .unwrap(),
            1
        );

        let committed = Rc::new(Cell::new(false));
        store
            .recover_pending_with_live(|pending| {
                assert_eq!(pending.skill_id, "docs");
                assert_eq!(pending.app, AppType::Claude);
                assert!(pending.enabled);
                Ok(FakeReceipt {
                    verified: true,
                    committed: committed.clone(),
                    rolled_back: Rc::new(Cell::new(false)),
                })
            })
            .unwrap()
            .unwrap();
        assert!(committed.get());
        assert_eq!(
            Connection::open(path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM skill_operation_journal", [], |row| {
                    row.get::<_, usize>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn failed_live_verification_rolls_back_the_catalog() {
        let (_directory, path, store) = seed_store();
        let rolled_back = Rc::new(Cell::new(false));
        let result = store
            .toggle_with_live("docs", AppType::Codex, true, |_| {
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
            let result = store.toggle_with_live::<FakeReceipt>(id, AppType::Claude, true, |_| {
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
            store.toggle_with_live::<FakeReceipt>("accent-a", AppType::Claude, true, |_| panic!(
                "aliased catalog rows must not reach live changes"
            )),
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
            .toggle_with_live("docs", AppType::Claude, false, |pending| {
                live.apply_skill_recoverable(
                    &pending.name,
                    &pending.directory,
                    &pending.app,
                    pending.enabled,
                )
            })
            .unwrap()
            .unwrap();
        assert!(!residual.exists());

        for enabled in [true, false] {
            store
                .toggle_with_live("docs", AppType::Claude, enabled, |pending| {
                    live.apply_skill_recoverable(
                        &pending.name,
                        &pending.directory,
                        &pending.app,
                        pending.enabled,
                    )
                })
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
        assert_eq!(
            Connection::open(path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM skill_operation_journal", [], |row| {
                    row.get::<_, usize>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn unified_gemini_skill_uses_its_native_disabled_list() {
        let directory = tempdir().unwrap();
        let shared = directory.path().join(".cc-switch");
        let source = shared.join("skills/docs");
        let unified = directory.path().join(".agents/skills/docs");
        let gemini = directory.path().join(".gemini");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&unified).unwrap();
        fs::create_dir_all(&gemini).unwrap();
        fs::write(source.join("SKILL.md"), "# Docs").unwrap();
        fs::write(unified.join("SKILL.md"), "# Docs").unwrap();
        fs::write(
            gemini.join("settings.json"),
            r#"{"theme":"dark","skills":{"disabled":["Docs"]}}"#,
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

        for enabled in [true, false] {
            store
                .toggle_with_live("docs", AppType::Gemini, enabled, |pending| {
                    live.apply_skill_recoverable(
                        &pending.name,
                        &pending.directory,
                        &pending.app,
                        pending.enabled,
                    )
                })
                .unwrap()
                .unwrap();
            let settings: serde_json::Value =
                serde_json::from_slice(&fs::read(gemini.join("settings.json")).unwrap()).unwrap();
            let disabled = settings["skills"]["disabled"].as_array().unwrap();
            assert_eq!(disabled.iter().any(|name| name == "Docs"), !enabled);
            assert!(!gemini.join("skills/docs").exists());
        }
    }
}
