use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Display,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use cc_switch_core::{
    builtin_app_registry, skill_directory_key, skill_name_key, AppType, SkillCopyPolicy,
    SkillSelectionMode,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior};
use serde::Serialize;
use thiserror::Error;

use crate::skill_host::{
    skill_host_adapter, skill_host_adapters, validate_skill_host_adapters, SkillHostAdapter,
};

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SKILL_SELECT_BASE: &str = "SELECT id, name, description, directory, repo_owner, repo_name";

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("Skill database failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("local Skill recovery state I/O failed for {path}: {source}")]
    LocalStateIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("shared Skill data is invalid: {0}")]
    InvalidStore(String),
    #[error("Skill is invalid: {0}")]
    InvalidSkill(String),
    #[error("Skill '{0}' was not found")]
    NotFound(String),
    #[error("shared Skill changed while its native configuration was being updated: {0}")]
    Conflict(String),
    #[error("shared Skill update failed and recovery was incomplete: {0}")]
    Recovery(String),
}

impl SkillError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_) | Self::LocalStateIo { .. } => "storage_error",
            Self::InvalidStore(_) => "invalid_store",
            Self::InvalidSkill(_) => "invalid_skill",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
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
    pub(crate) runtime_fingerprint: String,
    previous_enabled: Option<bool>,
    copy_policy: SkillCopyPolicy,
    phase: PendingPhase,
}

impl PendingSkillChange {
    pub(crate) fn copy_policy(&self) -> SkillCopyPolicy {
        self.copy_policy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPhase {
    Prepared,
    CatalogCommitted,
}

impl PendingPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::CatalogCommitted => "catalogCommitted",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "catalogCommitted" => Some(Self::CatalogCommitted),
            _ => None,
        }
    }
}

struct PendingScan {
    changes: Vec<PendingSkillChange>,
    issues: Vec<String>,
}

pub(crate) trait RecoverableSkillChange: Sized {
    type Error: Display;

    fn verify(&self) -> Result<(), Self::Error>;
    fn commit(&mut self) -> Result<(), Self::Error>;
    fn rollback(&mut self) -> Result<(), Self::Error>;

    fn recovery_incomplete(_error: &Self::Error) -> bool {
        false
    }
}

pub struct SkillStore {
    path: PathBuf,
    journal_path: PathBuf,
}

impl SkillStore {
    #[cfg(test)]
    pub fn open(path: PathBuf) -> Result<Self, SkillError> {
        let journal_path = path.with_file_name("cc-switch-lite-state.db");
        Self::open_with_local_state(path, journal_path)
    }

    pub(crate) fn open_with_local_state(
        path: PathBuf,
        journal_path: PathBuf,
    ) -> Result<Self, SkillError> {
        if path == journal_path {
            return Err(SkillError::InvalidStore(
                "shared Skill data and local recovery state must use separate files".to_owned(),
            ));
        }
        let store = Self { path, journal_path };
        store.initialize()?;
        if let Some(parent) = store.journal_path.parent() {
            fs::create_dir_all(parent).map_err(|source| SkillError::LocalStateIo {
                path: parent.to_owned(),
                source,
            })?;
        }
        ensure_separate_recovery_file(&store.path, &store.journal_path)?;
        store.initialize_journal()?;
        store.migrate_embedded_journal()?;
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
        drop(statement);
        let indexes = skills
            .iter()
            .enumerate()
            .map(|(index, skill)| (skill.id.clone(), index))
            .collect::<HashMap<_, _>>();
        let journal = self.connect_journal()?;
        let mut statement = journal.prepare(
            "SELECT CAST(skill_id AS TEXT), CAST(app_id AS TEXT)
             FROM skill_operation_journal",
        )?;
        let pending = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in pending {
            let (skill_id, app_id) = row?;
            let Some(skill) = indexes
                .get(&skill_id)
                .and_then(|index| skills.get_mut(*index))
            else {
                continue;
            };
            let Some(state) = skill.apps.get_mut(&app_id) else {
                continue;
            };
            state.issue =
                Some("A previous change is pending recovery; this switch is read-only".to_owned());
        }
        Ok(skills)
    }

    pub fn toggle_with_live<C>(
        &self,
        id: &str,
        app: AppType,
        enabled: bool,
        runtime_fingerprint: String,
        apply: impl FnOnce(&PendingSkillChange) -> Result<C, C::Error>,
    ) -> Result<Result<(), C::Error>, SkillError>
    where
        C: RecoverableSkillChange,
    {
        let pending = self.begin_toggle(id, app, enabled, runtime_fingerprint)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !pending_matches_catalog(&transaction, &pending)? {
            return Err(SkillError::Conflict(format!(
                "Skill '{}' or its '{}' selection changed before native configuration",
                pending.skill_id,
                pending.app.as_str()
            )));
        }
        let mut receipt = match apply(&pending) {
            Ok(receipt) => receipt,
            Err(error) => {
                if !C::recovery_incomplete(&error) {
                    restore_catalog(&transaction, &pending)?;
                    transaction.commit()?;
                    self.delete_pending(&pending)?;
                }
                return Ok(Err(error));
            }
        };
        if let Err(error) = receipt.verify() {
            return match receipt.rollback() {
                Ok(()) => {
                    restore_catalog(&transaction, &pending)?;
                    transaction.commit()?;
                    self.delete_pending(&pending)?;
                    Ok(Err(error))
                }
                Err(live) => Err(SkillError::Recovery(format_recovery(
                    "live verification",
                    &error,
                    None,
                    Some(live),
                ))),
            };
        }
        self.finalize_pending(&pending, transaction, receipt)
    }

    pub(crate) fn recover_pending_with_live<C>(
        &self,
        mut apply: impl FnMut(&PendingSkillChange) -> Result<C, C::Error>,
    ) -> Result<Vec<String>, SkillError>
    where
        C: RecoverableSkillChange,
    {
        let PendingScan {
            changes,
            mut issues,
        } = self.pending_changes()?;
        for pending in changes {
            let mut connection = match self.connect() {
                Ok(connection) => connection,
                Err(error) => {
                    issues.push(error.to_string());
                    continue;
                }
            };
            let transaction =
                match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
                    Ok(transaction) => transaction,
                    Err(error) => {
                        issues.push(error.to_string());
                        continue;
                    }
                };
            match pending_matches_catalog(&transaction, &pending) {
                Ok(true) => {}
                Ok(false) => {
                    issues.push(format!(
                        "pending Skill change for '{}' and '{}' no longer matches the shared catalog; recovery state was preserved",
                        pending.skill_id,
                        pending.app.as_str()
                    ));
                    continue;
                }
                Err(error) => {
                    issues.push(error.to_string());
                    continue;
                }
            }
            let mut receipt = match apply(&pending) {
                Ok(receipt) => receipt,
                Err(error) => {
                    issues.push(format!(
                        "could not recover '{}' for '{}': {error}",
                        pending.skill_id,
                        pending.app.as_str()
                    ));
                    continue;
                }
            };
            if let Err(error) = receipt.verify() {
                let issue = match receipt.rollback() {
                    Ok(()) => format!(
                        "could not verify recovery for '{}' and '{}': {error}",
                        pending.skill_id,
                        pending.app.as_str()
                    ),
                    Err(rollback) => {
                        format_recovery("pending live verification", &error, None, Some(rollback))
                    }
                };
                issues.push(issue);
                continue;
            }
            match self.finalize_pending(&pending, transaction, receipt) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => issues.push(format!(
                    "could not finalize recovery for '{}' and '{}': {error}",
                    pending.skill_id,
                    pending.app.as_str()
                )),
                Err(error) => issues.push(error.to_string()),
            }
        }
        Ok(issues)
    }

    fn begin_toggle(
        &self,
        id: &str,
        app: AppType,
        enabled: bool,
        runtime_fingerprint: String,
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
        let column = catalog_column(&app);
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
        if contract.config_target().is_some() {
            validate_unique_native_name(&transaction, &row.0)?;
        }
        let pending = PendingSkillChange {
            skill_id: id.to_owned(),
            name: row.0,
            directory: row.1,
            app,
            enabled,
            runtime_fingerprint,
            previous_enabled: row.2,
            copy_policy: if contract.selection() == SkillSelectionMode::HostManaged
                && row.2 == Some(true)
                && !enabled
            {
                SkillCopyPolicy::AllowMatching
            } else {
                SkillCopyPolicy::ManagedOnly
            },
            phase: PendingPhase::Prepared,
        };
        self.insert_pending(&pending)?;
        let database_result = (|| -> Result<(), SkillError> {
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
            Ok(())
        })();
        if let Err(error) = database_result {
            return match self.delete_pending_if_present(&pending) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(SkillError::Recovery(format!(
                    "shared catalog update failed: {error}; local journal cleanup failed: {cleanup}"
                ))),
            };
        }
        let mut pending = pending;
        self.mark_catalog_committed(&pending).map_err(|error| {
            SkillError::Recovery(format!(
                "shared catalog changed but its local recovery phase could not be recorded: {error}"
            ))
        })?;
        pending.phase = PendingPhase::CatalogCommitted;
        Ok(pending)
    }

    fn pending_changes(&self) -> Result<PendingScan, SkillError> {
        let journal = self.connect_journal()?;
        let mut statement = journal.prepare(
            "SELECT skill_id, app_id, skill_name, directory, enabled, runtime_fingerprint,
                    matching_copy_evidence, previous_enabled, phase
             FROM skill_operation_journal ORDER BY skill_id, app_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, Option<bool>>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        let rows = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(journal);
        let connection = self.connect()?;
        let mut changes = Vec::new();
        let mut stale = Vec::new();
        let mut issues = Vec::new();
        for row in rows {
            let resolved = (|| {
                let (
                    skill_id,
                    app_id,
                    name,
                    directory,
                    enabled,
                    runtime_fingerprint,
                    matching_copy_evidence,
                    previous_enabled,
                    phase,
                ) = row;
                let phase = PendingPhase::parse(&phase).ok_or_else(|| {
                    SkillError::InvalidStore(format!(
                        "pending Skill change for '{skill_id}' has invalid phase '{phase}'"
                    ))
                })?;
                let runtime_fingerprint = runtime_fingerprint
                    .filter(|fingerprint| !fingerprint.is_empty())
                    .ok_or_else(|| {
                        SkillError::InvalidStore(format!(
                            "pending Skill change for '{skill_id}' has no runtime fingerprint"
                        ))
                    })?;
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
                if contract.config_target().is_some() {
                    validate_unique_native_name(&connection, &name)?;
                }
                let mut catalog_matches_previous = false;
                if let Some(column) = catalog_column(&app) {
                    let desired = connection.query_row(
                        &format!("SELECT {column} FROM skills WHERE id = ?1"),
                        [&skill_id],
                        |row| row.get::<_, bool>(0),
                    )?;
                    if desired != enabled {
                        if previous_enabled == Some(desired) {
                            catalog_matches_previous = true;
                        } else {
                            return Err(SkillError::InvalidStore(format!(
                                "pending Skill change for '{skill_id}' does not match the catalog"
                            )));
                        }
                    }
                }
                let copy_policy = if matching_copy_evidence {
                    if contract.selection() != SkillSelectionMode::HostManaged
                        || enabled
                        || previous_enabled == Some(false)
                    {
                        return Err(SkillError::InvalidStore(format!(
                            "pending Skill change for '{skill_id}' has invalid copy evidence"
                        )));
                    }
                    SkillCopyPolicy::AllowMatching
                } else {
                    SkillCopyPolicy::ManagedOnly
                };
                let pending = PendingSkillChange {
                    skill_id,
                    name,
                    directory,
                    app,
                    enabled,
                    runtime_fingerprint,
                    previous_enabled,
                    copy_policy,
                    phase,
                };
                Ok((pending, catalog_matches_previous))
            })();
            match resolved {
                Ok((pending, true)) if pending.phase == PendingPhase::Prepared => {
                    stale.push(pending)
                }
                Ok((pending, true)) => issues.push(format!(
                    "pending Skill change for '{}' and '{}' may have reached native configuration after the catalog changed; recovery state was preserved",
                    pending.skill_id,
                    pending.app.as_str()
                )),
                Ok((pending, false)) => changes.push(pending),
                Err(error) => issues.push(error.to_string()),
            }
        }
        for pending in stale {
            self.delete_pending_if_present(&pending)?;
        }
        for pending in &mut changes {
            if pending.phase == PendingPhase::Prepared {
                self.mark_catalog_committed(pending)?;
                pending.phase = PendingPhase::CatalogCommitted;
            }
        }
        Ok(PendingScan { changes, issues })
    }

    #[cfg(test)]
    fn cancel_pending(&self, pending: &PendingSkillChange) -> Result<(), SkillError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        restore_catalog(&transaction, pending)?;
        transaction.commit()?;
        self.delete_pending(pending)?;
        Ok(())
    }

    fn finalize_pending<C>(
        &self,
        pending: &PendingSkillChange,
        transaction: rusqlite::Transaction<'_>,
        mut receipt: C,
    ) -> Result<Result<(), C::Error>, SkillError>
    where
        C: RecoverableSkillChange,
    {
        if !pending_matches_catalog(&transaction, pending)? {
            if let Err(error) = receipt.rollback() {
                return Err(SkillError::Recovery(format!(
                    "catalog changed and native rollback failed: {error}"
                )));
            }
            return Err(SkillError::Conflict(format!(
                "Skill '{}' or its '{}' selection changed; recovery state was preserved",
                pending.skill_id,
                pending.app.as_str()
            )));
        }
        if let Err(error) = receipt.commit() {
            return Ok(Err(error));
        }
        self.delete_pending(pending)?;
        transaction.commit()?;
        Ok(Ok(()))
    }

    fn initialize(&self) -> Result<(), SkillError> {
        validate_skill_host_adapters().map_err(SkillError::InvalidStore)?;
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
            );",
        )?;
        let mut columns = skill_columns(&transaction)?;
        verify_base_columns(&columns)?;
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

    fn initialize_journal(&self) -> Result<(), SkillError> {
        let mut connection = self.connect_journal()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS skill_operation_journal (
                skill_id TEXT NOT NULL,
                app_id TEXT NOT NULL,
                skill_name TEXT NOT NULL,
                directory TEXT NOT NULL,
                enabled BOOLEAN NOT NULL,
                runtime_fingerprint TEXT,
                matching_copy_evidence BOOLEAN NOT NULL DEFAULT 0,
                previous_enabled BOOLEAN,
                phase TEXT NOT NULL DEFAULT 'catalogCommitted',
                PRIMARY KEY (skill_id, app_id)
            );",
        )?;
        ensure_journal_extensions(&transaction)?;
        verify_journal_schema(&transaction)?;
        transaction.commit().map_err(Into::into)
    }

    fn migrate_embedded_journal(&self) -> Result<(), SkillError> {
        let mut shared = self.connect()?;
        let shared = shared.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = shared.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema
                WHERE type = 'table' AND name = 'skill_operation_journal'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Ok(());
        }
        let info = journal_schema_info(&shared)?;
        if !is_owned_embedded_journal(&info) {
            return Ok(());
        }
        let rows = {
            let mut statement = shared.prepare(
                "SELECT skill_id, app_id, skill_name, directory, enabled,
                        runtime_fingerprint, matching_copy_evidence
                 FROM skill_operation_journal ORDER BY skill_id, app_id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, bool>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, bool>(6)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut journal = self.connect_journal()?;
        let journal = journal.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for row in &rows {
            journal.execute(
                "INSERT OR IGNORE INTO skill_operation_journal
                    (skill_id, app_id, skill_name, directory, enabled, runtime_fingerprint,
                     matching_copy_evidence, previous_enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                params![row.0, row.1, row.2, row.3, row.4, row.5, row.6],
            )?;
            let copied = journal.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM skill_operation_journal
                    WHERE skill_id = ?1 AND app_id = ?2 AND skill_name = ?3
                      AND directory = ?4 AND enabled = ?5 AND runtime_fingerprint IS ?6
                      AND matching_copy_evidence = ?7 AND previous_enabled IS NULL
                 )",
                params![row.0, row.1, row.2, row.3, row.4, row.5, row.6],
                |row| row.get::<_, bool>(0),
            )?;
            if !copied {
                return Err(SkillError::Recovery(format!(
                    "local Skill recovery state conflicts with the previous embedded journal for '{}' and '{}'",
                    row.0, row.1
                )));
            }
        }
        journal.commit()?;
        shared.execute_batch("DROP TABLE skill_operation_journal")?;
        shared.commit().map_err(Into::into)
    }

    fn insert_pending(&self, pending: &PendingSkillChange) -> Result<(), SkillError> {
        let mut connection = self.connect_journal()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO skill_operation_journal
                (skill_id, app_id, skill_name, directory, enabled, runtime_fingerprint,
                 matching_copy_evidence, previous_enabled, phase)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                pending.skill_id,
                pending.app.as_str(),
                pending.name,
                pending.directory,
                pending.enabled,
                pending.runtime_fingerprint,
                pending.copy_policy == SkillCopyPolicy::AllowMatching,
                pending.previous_enabled,
                pending.phase.as_str(),
            ],
        )?;
        transaction.commit().map_err(Into::into)
    }

    fn mark_catalog_committed(&self, pending: &PendingSkillChange) -> Result<(), SkillError> {
        let mut connection = self.connect_journal()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE skill_operation_journal SET phase = ?1
             WHERE skill_id = ?2 AND app_id = ?3 AND skill_name = ?4
               AND directory = ?5 AND enabled = ?6 AND runtime_fingerprint = ?7
               AND matching_copy_evidence = ?8 AND previous_enabled IS ?9
               AND phase = ?10",
            params![
                PendingPhase::CatalogCommitted.as_str(),
                pending.skill_id,
                pending.app.as_str(),
                pending.name,
                pending.directory,
                pending.enabled,
                pending.runtime_fingerprint,
                pending.copy_policy == SkillCopyPolicy::AllowMatching,
                pending.previous_enabled,
                PendingPhase::Prepared.as_str(),
            ],
        )?;
        if updated != 1 {
            return Err(SkillError::Recovery(format!(
                "pending Skill phase changed for '{}' and '{}'",
                pending.skill_id,
                pending.app.as_str()
            )));
        }
        transaction.commit().map_err(Into::into)
    }

    fn delete_pending(&self, pending: &PendingSkillChange) -> Result<(), SkillError> {
        let mut connection = self.connect_journal()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = delete_pending_row(&transaction, pending)?;
        if deleted != 1 {
            return Err(SkillError::Recovery(format!(
                "pending Skill change disappeared for '{}' and '{}'",
                pending.skill_id,
                pending.app.as_str()
            )));
        }
        transaction.commit().map_err(Into::into)
    }

    fn delete_pending_if_present(&self, pending: &PendingSkillChange) -> Result<(), SkillError> {
        let mut connection = self.connect_journal()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        delete_pending_row(&transaction, pending)?;
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

    fn connect_journal(&self) -> Result<Connection, SkillError> {
        let connection = Connection::open_with_flags(
            &self.journal_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        Ok(connection)
    }
}

fn ensure_separate_recovery_file(shared: &Path, journal: &Path) -> Result<(), SkillError> {
    let metadata = match fs::symlink_metadata(journal) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SkillError::LocalStateIo {
                path: journal.to_owned(),
                source,
            })
        }
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(SkillError::InvalidStore(
            "local Skill recovery state must be a regular non-symbolic file".to_owned(),
        ));
    }
    match same_file::is_same_file(shared, journal) {
        Ok(true) => Err(SkillError::InvalidStore(
            "shared Skill data and local recovery state resolve to the same file".to_owned(),
        )),
        Ok(false) => Ok(()),
        Err(source) => Err(SkillError::LocalStateIo {
            path: journal.to_owned(),
            source,
        }),
    }
}

struct CatalogBinding {
    app_id: &'static str,
    column: &'static str,
}

fn catalog_column(app: &AppType) -> Option<&'static str> {
    skill_host_adapter(app).and_then(SkillHostAdapter::catalog_column)
}

fn catalog_bindings() -> Vec<CatalogBinding> {
    skill_host_adapters()
        .iter()
        .filter_map(|adapter| {
            adapter.catalog_column().map(|column| CatalogBinding {
                app_id: adapter.app().as_str(),
                column,
            })
        })
        .collect()
}

fn row_to_skill(row: &Row<'_>, bindings: &[CatalogBinding]) -> rusqlite::Result<SkillRecord> {
    let mut apps = skill_host_adapters()
        .iter()
        .map(|adapter| {
            let enabled = adapter.catalog_column().map(|_| false);
            (
                adapter.app().as_str().to_owned(),
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

fn validate_unique_native_name(connection: &Connection, name: &str) -> Result<(), SkillError> {
    let expected =
        skill_name_key(name).map_err(|error| SkillError::InvalidSkill(error.to_string()))?;
    let mut statement = connection.prepare("SELECT name FROM skills")?;
    let names = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut count = 0;
    for name in names {
        if skill_name_key(&name?).is_ok_and(|key| key == expected) {
            count += 1;
        }
    }
    if count != 1 {
        return Err(SkillError::InvalidSkill(format!(
            "native Skill name '{name}' is not unique"
        )));
    }
    Ok(())
}

fn pending_matches_catalog(
    connection: &Connection,
    pending: &PendingSkillChange,
) -> Result<bool, SkillError> {
    let contract = builtin_app_registry()
        .for_app(&pending.app)
        .skill_contract()
        .ok_or_else(|| {
            SkillError::InvalidStore(format!(
                "application '{}' no longer supports Skills",
                pending.app.as_str()
            ))
        })?;
    let query = catalog_column(&pending.app).map_or_else(
        || "SELECT name, directory, NULL FROM skills WHERE id = ?1".to_owned(),
        |column| format!("SELECT name, directory, {column} FROM skills WHERE id = ?1"),
    );
    let current = connection
        .query_row(&query, [&pending.skill_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<bool>>(2)?,
            ))
        })
        .optional()?;
    let Some((name, directory, enabled)) = current else {
        return Ok(false);
    };
    if name != pending.name || directory != pending.directory {
        return Ok(false);
    }
    if count_directory_key(
        connection,
        &skill_directory_key(&directory)
            .map_err(|error| SkillError::InvalidSkill(error.to_string()))?,
    )? != 1
    {
        return Ok(false);
    }
    if contract.config_target().is_some() && validate_unique_native_name(connection, &name).is_err()
    {
        return Ok(false);
    }
    Ok(catalog_column(&pending.app).is_none_or(|_| enabled == Some(pending.enabled)))
}

fn restore_catalog(
    connection: &Connection,
    pending: &PendingSkillChange,
) -> Result<(), SkillError> {
    let Some(previous) = pending.previous_enabled else {
        return Ok(());
    };
    let column = catalog_column(&pending.app).ok_or_else(|| {
        SkillError::Recovery(format!(
            "missing catalog binding for '{}'",
            pending.app.as_str()
        ))
    })?;
    let updated = connection.execute(
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
    Ok(())
}

fn delete_pending_row(
    connection: &Connection,
    pending: &PendingSkillChange,
) -> Result<usize, SkillError> {
    connection
        .execute(
            "DELETE FROM skill_operation_journal
         WHERE skill_id = ?1 AND app_id = ?2 AND skill_name = ?3
           AND directory = ?4 AND enabled = ?5 AND runtime_fingerprint = ?6
           AND matching_copy_evidence = ?7 AND previous_enabled IS ?8 AND phase = ?9",
            params![
                pending.skill_id,
                pending.app.as_str(),
                pending.name,
                pending.directory,
                pending.enabled,
                pending.runtime_fingerprint,
                pending.copy_policy == SkillCopyPolicy::AllowMatching,
                pending.previous_enabled,
                pending.phase.as_str(),
            ],
        )
        .map_err(Into::into)
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

fn ensure_journal_extensions(connection: &Connection) -> Result<(), SkillError> {
    let info = journal_schema_info(connection)?;
    verify_journal_info(&info, false)?;
    let columns = info
        .iter()
        .map(|column| column.name.as_str())
        .collect::<HashSet<_>>();
    if !columns.contains("runtime_fingerprint") {
        connection.execute_batch(
            "ALTER TABLE skill_operation_journal ADD COLUMN runtime_fingerprint TEXT",
        )?;
    }
    if !columns.contains("matching_copy_evidence") {
        connection.execute_batch(
            "ALTER TABLE skill_operation_journal
             ADD COLUMN matching_copy_evidence BOOLEAN NOT NULL DEFAULT 0",
        )?;
    }
    if !columns.contains("previous_enabled") {
        connection.execute_batch(
            "ALTER TABLE skill_operation_journal ADD COLUMN previous_enabled BOOLEAN",
        )?;
    }
    if !columns.contains("phase") {
        connection.execute_batch(
            "ALTER TABLE skill_operation_journal
             ADD COLUMN phase TEXT NOT NULL DEFAULT 'catalogCommitted'",
        )?;
    }
    Ok(())
}

fn verify_journal_schema(connection: &Connection) -> Result<(), SkillError> {
    verify_journal_info(&journal_schema_info(connection)?, true)
}

struct JournalColumnInfo {
    name: String,
    declared_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key: usize,
}

fn journal_schema_info(connection: &Connection) -> Result<Vec<JournalColumnInfo>, SkillError> {
    let mut statement = connection.prepare("PRAGMA table_info(skill_operation_journal)")?;
    let info = statement
        .query_map([], |row| {
            Ok(JournalColumnInfo {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get(3)?,
                default_value: row.get(4)?,
                primary_key: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SkillError::from)?;
    Ok(info)
}

fn verify_journal_info(
    info: &[JournalColumnInfo],
    require_runtime_fingerprint: bool,
) -> Result<(), SkillError> {
    let columns = info
        .iter()
        .map(|column| column.name.as_str())
        .collect::<HashSet<_>>();
    let mut required = vec!["skill_id", "app_id", "skill_name", "directory", "enabled"];
    if require_runtime_fingerprint {
        required.push("runtime_fingerprint");
        required.push("matching_copy_evidence");
        required.push("previous_enabled");
        required.push("phase");
    }
    for required in required {
        if !columns.contains(required) {
            return Err(SkillError::InvalidStore(format!(
                "skill_operation_journal is missing required column '{required}'"
            )));
        }
    }
    let primary_key = info
        .iter()
        .filter(|column| column.primary_key > 0)
        .map(|column| (column.name.as_str(), column.primary_key))
        .collect::<Vec<_>>();
    if primary_key != [("skill_id", 1), ("app_id", 2)] {
        return Err(SkillError::InvalidStore(
            "skill_operation_journal has an incompatible primary key".to_owned(),
        ));
    }
    Ok(())
}

fn is_owned_embedded_journal(info: &[JournalColumnInfo]) -> bool {
    let expected = [
        ("skill_id", "TEXT", true, None, 1),
        ("app_id", "TEXT", true, None, 2),
        ("skill_name", "TEXT", true, None, 0),
        ("directory", "TEXT", true, None, 0),
        ("enabled", "BOOLEAN", true, None, 0),
        ("runtime_fingerprint", "TEXT", false, None, 0),
        ("matching_copy_evidence", "BOOLEAN", true, Some("0"), 0),
    ];
    info.len() == expected.len()
        && info.iter().zip(expected).all(|(column, expected)| {
            column.name == expected.0
                && column.declared_type.eq_ignore_ascii_case(expected.1)
                && column.not_null == expected.2
                && column.default_value.as_deref() == expected.3
                && column.primary_key == expected.4
        })
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

        fn commit(&mut self) -> Result<(), Self::Error> {
            self.committed.set(true);
            Ok(())
        }

        fn rollback(&mut self) -> Result<(), Self::Error> {
            self.rolled_back.set(true);
            Ok(())
        }

        fn recovery_incomplete(error: &Self::Error) -> bool {
            *error == "recovery incomplete"
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

    fn journal_count(store: &SkillStore) -> usize {
        Connection::open(&store.journal_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM skill_operation_journal", [], |row| {
                row.get(0)
            })
            .unwrap()
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
        let journal_path = store.journal_path.clone();
        drop(store);
        Connection::open(journal_path)
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
    fn recovery_database_hardlink_to_shared_database_is_rejected() {
        let (directory, path, store) = seed_store();
        drop(store);
        let alias = directory.path().join("recovery-alias.db");
        fs::hard_link(&path, &alias).unwrap();

        assert!(matches!(
            SkillStore::open_with_local_state(path, alias),
            Err(SkillError::InvalidStore(_))
        ));
    }

    #[test]
    fn legacy_journal_rows_without_runtime_identity_stay_read_only() {
        let (_directory, path, store) = seed_store();
        let journal_path = store.journal_path.clone();
        drop(store);
        Connection::open(&journal_path)
            .unwrap()
            .execute_batch(
                "DROP TABLE skill_operation_journal;
                 CREATE TABLE skill_operation_journal (
                    skill_id TEXT NOT NULL,
                    app_id TEXT NOT NULL,
                    skill_name TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    enabled BOOLEAN NOT NULL,
                    PRIMARY KEY (skill_id, app_id)
                 );
                 INSERT INTO skill_operation_journal
                    (skill_id, app_id, skill_name, directory, enabled)
                 VALUES ('docs', 'claude', 'Docs', 'docs', 1);",
            )
            .unwrap();

        let store = SkillStore::open(path.clone()).unwrap();
        let issues = store
            .recover_pending_with_live::<FakeReceipt>(|_| {
                panic!("a legacy row must not run against unbound paths")
            })
            .unwrap();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("runtime fingerprint"));
        assert_eq!(
            Connection::open(journal_path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM skill_operation_journal", [], |row| {
                    row.get::<_, usize>(0)
                })
                .unwrap(),
            1
        );
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
        assert!(!connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table' AND name = 'skill_operation_journal'
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
        assert_eq!(journal_count(&store), 0);
    }

    #[test]
    fn previous_embedded_journal_moves_to_local_state_once() {
        let (_directory, path, store) = seed_store();
        drop(store);
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE skill_operation_journal (
                    skill_id TEXT NOT NULL,
                    app_id TEXT NOT NULL,
                    skill_name TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    enabled BOOLEAN NOT NULL,
                    runtime_fingerprint TEXT,
                    matching_copy_evidence BOOLEAN NOT NULL DEFAULT 0,
                    PRIMARY KEY (skill_id, app_id)
                 );
                 UPDATE skills SET enabled_claude = 1 WHERE id = 'docs';
                 INSERT INTO skill_operation_journal
                    (skill_id, app_id, skill_name, directory, enabled,
                     runtime_fingerprint, matching_copy_evidence)
                 VALUES ('docs', 'claude', 'Docs', 'docs', 1, 'runtime-v1', 0);",
            )
            .unwrap();

        let store = SkillStore::open(path.clone()).unwrap();

        assert_eq!(journal_count(&store), 1);
        assert!(!Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table' AND name = 'skill_operation_journal'
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
    }

    #[test]
    fn toggle_uses_the_core_catalog_binding_only() {
        let (_directory, path, store) = seed_store();
        let committed = Rc::new(Cell::new(false));
        let rolled_back = Rc::new(Cell::new(false));
        store
            .toggle_with_live(
                "docs",
                AppType::Claude,
                true,
                "runtime-v1".to_owned(),
                |pending| {
                    assert_eq!(pending.directory, "docs");
                    assert_eq!(pending.name, "Docs");
                    assert_eq!(pending.app, AppType::Claude);
                    assert!(pending.enabled);
                    Ok(FakeReceipt {
                        verified: true,
                        committed: committed.clone(),
                        rolled_back: rolled_back.clone(),
                    })
                },
            )
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
    fn unverified_embedded_journal_is_never_claimed_or_dropped() {
        let (_directory, path, store) = seed_store();
        drop(store);
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE skill_operation_journal (
                    skill_id TEXT NOT NULL,
                    app_id TEXT NOT NULL,
                    skill_name TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    enabled INTEGER NOT NULL,
                    runtime_fingerprint TEXT,
                    matching_copy_evidence BOOLEAN NOT NULL DEFAULT 0,
                    PRIMARY KEY (skill_id, app_id)
                 );",
            )
            .unwrap();

        SkillStore::open(path.clone()).unwrap();

        assert!(Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table' AND name = 'skill_operation_journal'
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
    }

    #[test]
    fn durable_intent_is_replayed_after_an_interrupted_toggle() {
        let (_directory, _path, store) = seed_store();
        let pending = store
            .begin_toggle("docs", AppType::Claude, true, "runtime-v1".to_owned())
            .unwrap();
        assert!(pending.enabled);
        assert_eq!(journal_count(&store), 1);

        let committed = Rc::new(Cell::new(false));
        let issues = store
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
            .unwrap();
        assert!(issues.is_empty());
        assert!(committed.get());
        assert_eq!(journal_count(&store), 0);
    }

    #[test]
    fn prepared_intent_left_before_catalog_commit_is_discarded_without_live_changes() {
        let (_directory, path, store) = seed_store();
        store
            .begin_toggle("docs", AppType::Claude, true, "runtime-v1".to_owned())
            .unwrap();
        Connection::open(path)
            .unwrap()
            .execute("UPDATE skills SET enabled_claude = 0 WHERE id = 'docs'", [])
            .unwrap();
        Connection::open(&store.journal_path)
            .unwrap()
            .execute("UPDATE skill_operation_journal SET phase = 'prepared'", [])
            .unwrap();

        let issues = store
            .recover_pending_with_live::<FakeReceipt>(|_| {
                panic!("an intent without its catalog commit must not reach live files")
            })
            .unwrap();

        assert!(issues.is_empty());
        assert_eq!(journal_count(&store), 0);
    }

    #[test]
    fn catalog_committed_intent_is_preserved_when_catalog_returns_to_previous_state() {
        let (_directory, path, store) = seed_store();
        store
            .begin_toggle("docs", AppType::Claude, true, "runtime-v1".to_owned())
            .unwrap();
        Connection::open(path)
            .unwrap()
            .execute("UPDATE skills SET enabled_claude = 0 WHERE id = 'docs'", [])
            .unwrap();

        let issues = store
            .recover_pending_with_live::<FakeReceipt>(|_| {
                panic!("an ambiguous committed intent must not reach live files")
            })
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(journal_count(&store), 1);
    }

    #[test]
    fn matching_copy_evidence_is_explicit_and_durable() {
        let (_directory, path, store) = seed_store();
        let pending = store
            .begin_toggle("docs", AppType::Claude, false, "runtime-v1".to_owned())
            .unwrap();
        assert_eq!(pending.copy_policy(), SkillCopyPolicy::ManagedOnly);
        store.cancel_pending(&pending).unwrap();

        Connection::open(&path)
            .unwrap()
            .execute("UPDATE skills SET enabled_claude = 1", [])
            .unwrap();
        let pending = store
            .begin_toggle("docs", AppType::Claude, false, "runtime-v1".to_owned())
            .unwrap();
        assert_eq!(pending.copy_policy(), SkillCopyPolicy::AllowMatching);

        let recovered = store.pending_changes().unwrap();
        assert!(recovered.issues.is_empty());
        assert_eq!(recovered.changes.len(), 1);
        assert_eq!(
            recovered.changes[0].copy_policy(),
            SkillCopyPolicy::AllowMatching
        );
    }

    #[test]
    fn recovery_isolates_a_failed_entry_and_commits_the_next_one() {
        let (_directory, path, store) = seed_store();
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO skills (id, name, directory) VALUES ('tools', 'Tools', 'tools')",
                [],
            )
            .unwrap();
        store
            .begin_toggle("docs", AppType::Claude, true, "runtime-v1".to_owned())
            .unwrap();
        store
            .begin_toggle("tools", AppType::Claude, true, "runtime-v1".to_owned())
            .unwrap();
        let committed = Rc::new(Cell::new(false));

        let issues = store
            .recover_pending_with_live(|pending| {
                if pending.skill_id == "docs" {
                    return Err("source unavailable");
                }
                Ok(FakeReceipt {
                    verified: true,
                    committed: committed.clone(),
                    rolled_back: Rc::new(Cell::new(false)),
                })
            })
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert!(committed.get());
        assert_eq!(journal_count(&store), 1);
        assert!(store.list().unwrap()[0].apps["claude"].issue.is_some());
    }

    #[test]
    fn incomplete_apply_recovery_keeps_the_durable_intent() {
        let (_directory, path, store) = seed_store();
        let result = store
            .toggle_with_live::<FakeReceipt>(
                "docs",
                AppType::Claude,
                true,
                "runtime-v1".to_owned(),
                |_| Err("recovery incomplete"),
            )
            .unwrap();

        assert_eq!(result, Err("recovery incomplete"));
        let connection = Connection::open(path).unwrap();
        assert!(connection
            .query_row("SELECT enabled_claude FROM skills", [], |row| row
                .get::<_, bool>(0))
            .unwrap());
        assert_eq!(journal_count(&store), 1);
    }

    #[test]
    fn failed_live_verification_rolls_back_the_catalog() {
        let (_directory, path, store) = seed_store();
        let rolled_back = Rc::new(Cell::new(false));
        let result = store
            .toggle_with_live(
                "docs",
                AppType::Codex,
                true,
                "runtime-v1".to_owned(),
                |_| {
                    Ok(FakeReceipt {
                        verified: false,
                        committed: Rc::new(Cell::new(false)),
                        rolled_back: rolled_back.clone(),
                    })
                },
            )
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
    fn shared_write_transaction_is_held_before_live_configuration() {
        let (_directory, path, store) = seed_store();
        let path_for_apply = path.clone();
        let observed_lock = Rc::new(Cell::new(false));
        let observed_lock_for_apply = observed_lock.clone();

        store
            .toggle_with_live(
                "docs",
                AppType::Claude,
                true,
                "runtime-v1".to_owned(),
                |_| {
                    let connection = Connection::open(&path_for_apply).unwrap();
                    connection.busy_timeout(Duration::ZERO).unwrap();
                    let locked = connection
                        .execute(
                            "UPDATE skills SET updated_at = updated_at + 1 WHERE id = 'docs'",
                            [],
                        )
                        .is_err();
                    observed_lock_for_apply.set(locked);
                    Ok(FakeReceipt {
                        verified: true,
                        committed: Rc::new(Cell::new(false)),
                        rolled_back: Rc::new(Cell::new(false)),
                    })
                },
            )
            .unwrap()
            .unwrap();

        assert!(observed_lock.get());
        assert_eq!(journal_count(&store), 0);
    }

    #[test]
    fn catalog_rollback_commits_before_journal_cleanup() {
        let (_directory, path, store) = seed_store();
        let pending = store
            .begin_toggle("docs", AppType::Claude, true, "runtime-v1".to_owned())
            .unwrap();
        Connection::open(&store.journal_path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_skill_journal_delete
                 BEFORE DELETE ON skill_operation_journal
                 BEGIN SELECT RAISE(ABORT, 'keep journal'); END;",
            )
            .unwrap();

        assert!(matches!(
            store.cancel_pending(&pending),
            Err(SkillError::Database(_))
        ));
        assert!(!Connection::open(path)
            .unwrap()
            .query_row("SELECT enabled_claude FROM skills", [], |row| row
                .get::<_, bool>(0))
            .unwrap());
        assert_eq!(journal_count(&store), 1);
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
            let result = store.toggle_with_live::<FakeReceipt>(
                id,
                AppType::Claude,
                true,
                "runtime-v1".to_owned(),
                |_| panic!("invalid catalog rows must not reach live changes"),
            );
            assert!(matches!(result, Err(SkillError::InvalidSkill(_))));
        }
    }

    #[test]
    fn duplicate_native_names_are_read_only_for_name_based_controls() {
        let (_directory, path, store) = seed_store();
        Connection::open(path)
            .unwrap()
            .execute(
                "INSERT INTO skills (id, name, directory) VALUES ('other', 'Ｄocs', 'other')",
                [],
            )
            .unwrap();

        assert!(matches!(
            store.toggle_with_live::<FakeReceipt>(
                "docs",
                AppType::Gemini,
                true,
                "runtime-v1".to_owned(),
                |_| panic!("ambiguous names must not reach native controls")
            ),
            Err(SkillError::InvalidSkill(_))
        ));
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
                "runtime-v1".to_owned(),
                |_| panic!("aliased catalog rows must not reach live changes")
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
        cc_switch_core::apply_skill_deployment(
            &shared.join("skills"),
            &claude.join("skills"),
            "docs",
            true,
            cc_switch_core::SkillSyncMethod::Copy,
        )
        .unwrap()
        .commit()
        .unwrap();
        let runtime_fingerprint = live.skill_runtime_fingerprint(&AppType::Claude).unwrap();
        store
            .toggle_with_live(
                "docs",
                AppType::Claude,
                false,
                runtime_fingerprint,
                |pending| {
                    live.apply_skill_recoverable(
                        &pending.name,
                        &pending.directory,
                        &pending.app,
                        pending.enabled,
                        &pending.runtime_fingerprint,
                        pending.copy_policy(),
                    )
                },
            )
            .unwrap()
            .unwrap();
        assert!(!residual.exists());

        for enabled in [true, false] {
            let runtime_fingerprint = live.skill_runtime_fingerprint(&AppType::Claude).unwrap();
            store
                .toggle_with_live(
                    "docs",
                    AppType::Claude,
                    enabled,
                    runtime_fingerprint,
                    |pending| {
                        live.apply_skill_recoverable(
                            &pending.name,
                            &pending.directory,
                            &pending.app,
                            pending.enabled,
                            &pending.runtime_fingerprint,
                            pending.copy_policy(),
                        )
                    },
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
        assert_eq!(journal_count(&store), 0);
    }

    #[test]
    fn pending_change_never_replays_against_new_path_settings() {
        let directory = tempdir().unwrap();
        let shared = directory.path().join(".cc-switch");
        let source = shared.join("skills/docs");
        let initial_claude = directory.path().join("claude-initial");
        let current_claude = directory.path().join("claude-current");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&initial_claude).unwrap();
        fs::create_dir_all(&current_claude).unwrap();
        fs::write(source.join("SKILL.md"), "# Docs").unwrap();
        fs::write(
            shared.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "claudeConfigDir": initial_claude,
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
        let initial_fingerprint = live.skill_runtime_fingerprint(&AppType::Claude).unwrap();
        store
            .begin_toggle("docs", AppType::Claude, true, initial_fingerprint)
            .unwrap();

        fs::write(
            shared.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "claudeConfigDir": current_claude,
                "skillSyncMethod": "copy"
            }))
            .unwrap(),
        )
        .unwrap();
        let issues = store
            .recover_pending_with_live(|pending| {
                live.apply_skill_recoverable(
                    &pending.name,
                    &pending.directory,
                    &pending.app,
                    pending.enabled,
                    &pending.runtime_fingerprint,
                    pending.copy_policy(),
                )
            })
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert!(!initial_claude.join("skills/docs").exists());
        assert!(!current_claude.join("skills/docs").exists());
        assert_eq!(journal_count(&store), 1);
    }

    #[test]
    fn unified_gemini_skill_uses_its_native_disabled_list() {
        let directory = tempdir().unwrap();
        let shared = directory.path().join(".cc-switch");
        let source = shared.join("skills/docs");
        let unified_root = directory.path().join(".agents/skills");
        let gemini = directory.path().join(".gemini");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&gemini).unwrap();
        fs::write(source.join("SKILL.md"), "# Docs").unwrap();
        cc_switch_core::apply_skill_deployment(
            &shared.join("skills"),
            &unified_root,
            "docs",
            true,
            cc_switch_core::SkillSyncMethod::Copy,
        )
        .unwrap()
        .commit()
        .unwrap();
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
            let runtime_fingerprint = live.skill_runtime_fingerprint(&AppType::Gemini).unwrap();
            store
                .toggle_with_live(
                    "docs",
                    AppType::Gemini,
                    enabled,
                    runtime_fingerprint,
                    |pending| {
                        live.apply_skill_recoverable(
                            &pending.name,
                            &pending.directory,
                            &pending.app,
                            pending.enabled,
                            &pending.runtime_fingerprint,
                            pending.copy_policy(),
                        )
                    },
                )
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
