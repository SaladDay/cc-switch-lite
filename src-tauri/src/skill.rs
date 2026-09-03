use std::path::PathBuf;

use cc_switch_core::{AppType, InstalledSkillSnapshot, SkillCatalogDecision, SkillControlReason};
use cc_switch_store::{
    apply_skill_catalog_plan, begin_immediate_transaction, read_skill_catalog,
    read_skill_catalog_entry, SharedDatabase, SharedStoreError, SkillCatalogWriteOutcome,
};
use thiserror::Error;

use crate::{
    live::{LiveConfig, SkillWriteReceipt},
    skill_live::SkillHostError,
};

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("shared Skill database failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("shared Skill data is invalid: {0}")]
    InvalidStore(String),
    #[error(transparent)]
    SharedWrite(SharedStoreError),
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
            Self::Database(_) | Self::Io { .. } | Self::SharedWrite(_) => "storage_error",
            Self::InvalidStore(_) => "invalid_store",
            Self::Conflict => "conflict",
            Self::Host(error) => error.code(),
            Self::Recovery(_) => "recovery_failed",
        }
    }
}

impl From<SharedStoreError> for SkillError {
    fn from(error: SharedStoreError) -> Self {
        match error {
            SharedStoreError::Io { path, source } => Self::Io { path, source },
            SharedStoreError::Database(error) => Self::Database(error),
            SharedStoreError::InvalidDatabase(message) => Self::InvalidStore(message),
            other => Self::SharedWrite(other),
        }
    }
}

pub struct SkillStore {
    database: SharedDatabase,
}

impl SkillStore {
    pub fn open(path: PathBuf) -> Result<Self, SkillError> {
        let database = SharedDatabase::open(path)?;
        database.ensure_skill_schema()?;
        Ok(Self { database })
    }

    pub fn list(&self, live: &LiveConfig) -> Result<Vec<InstalledSkillSnapshot>, SkillError> {
        let catalog = read_skill_catalog(&self.connect()?)?;
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

    pub fn reconcile_and_list(
        &self,
        live: &LiveConfig,
    ) -> Result<(Vec<InstalledSkillSnapshot>, Vec<String>), SkillError> {
        let snapshots = self.list(live)?;
        let pending = snapshots
            .iter()
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
        if pending.is_empty() {
            return Ok((snapshots, Vec::new()));
        }
        let failures = pending
            .into_iter()
            .filter_map(|(skill_id, app)| {
                self.change(live, &skill_id, app.clone(), None)
                    .err()
                    .map(|error| format!("{skill_id}/{}: {error}", app.as_str()))
            })
            .collect::<Vec<_>>();
        let snapshots = if failures.is_empty() {
            self.list(live)?
        } else {
            snapshots
        };
        Ok((snapshots, failures))
    }

    fn change(
        &self,
        live: &LiveConfig,
        skill_id: &str,
        app: AppType,
        requested: Option<bool>,
    ) -> Result<(), SkillError> {
        let mut connection = self.connect()?;
        let mut transaction = begin_immediate_transaction(&mut connection)?;
        let catalog = read_skill_catalog(&transaction)?;
        let receipt = live.apply_skill_recoverable(&catalog, skill_id, &app, requested)?;

        let database_result = (|| -> Result<(), SkillError> {
            require_applied(apply_skill_catalog_plan(
                &mut transaction,
                receipt.value.plan(),
            )?)?;
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
            .and_then(|connection| {
                read_skill_catalog_entry(&connection, skill_id).map_err(Into::into)
            })
            .map(|entry| receipt.value.decide_catalog(entry.as_ref()));
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

    fn connect(&self) -> Result<rusqlite::Connection, SkillError> {
        self.database.connect().map_err(Into::into)
    }
}

fn require_applied(outcome: SkillCatalogWriteOutcome) -> Result<(), SkillError> {
    match outcome {
        SkillCatalogWriteOutcome::Applied => Ok(()),
        SkillCatalogWriteOutcome::NotApplied => Err(SkillError::Conflict),
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

#[cfg(test)]
mod tests {
    use super::*;
    use cc_switch_core::skill_catalog_columns;
    use rusqlite::Connection;
    use std::{fs, path::Path};
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

        let catalog = read_skill_catalog(&store.connect().unwrap()).unwrap();
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

        let (snapshots, failures) = store.reconcile_and_list(&live).unwrap();
        assert!(failures.is_empty());
        let claude = snapshots[0]
            .apps()
            .find(|state| state.app() == &AppType::Claude)
            .unwrap();
        assert_eq!(claude.selected(), Some(true));
        assert_eq!(claude.enabled(), Some(true));
        assert_eq!(claude.reason(), None);
    }
}
