use std::{
    fs,
    path::{Path, PathBuf},
};

use cc_switch_core::{
    builtin_app_registry, execute_skill_live_plan, inspect_installed_skills,
    prepare_skill_reconciliation, prepare_skill_switch, AppType, InstalledSkillSnapshot,
    ObservedDocument, SkillAppRuntime, SkillCatalogDecision, SkillCatalogEntry, SkillLiveReceipt,
    SkillRuntime, SkillSwitchPlan,
};
use thiserror::Error;

use crate::{
    live::ResolvedConfigDirs,
    operation::{read_optional, FileOperationHost, LivePaths, OperationError},
};

const STATE_DIRECTORY: &str = ".cc-switch-skill-references";

#[derive(Debug, Error)]
pub enum SkillHostError {
    #[error("application '{0}' does not support Skills")]
    UnsupportedApp(String),
    #[error("Skill storage setting is invalid: {0}")]
    InvalidStorage(String),
    #[error("Skill runtime is invalid: {0}")]
    Runtime(String),
    #[error("Skill catalog could not be observed: {0}")]
    Observation(String),
    #[error("Skill switch could not be prepared: {0}")]
    Prepare(String),
    #[error("Skill live update failed: {0}")]
    Live(String),
    #[error("live configuration lock is unavailable")]
    LockUnavailable,
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error("Skill directory I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl SkillHostError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedApp(_) | Self::InvalidStorage(_) | Self::Runtime(_) => {
                "invalid_skill_runtime"
            }
            Self::Observation(_) => "skill_observation_failed",
            Self::Prepare(_) => "skill_unavailable",
            Self::Live(_) => "skill_live_failed",
            Self::LockUnavailable => "lock_unavailable",
            Self::Operation(error) => error.code(),
            Self::Io { .. } => "live_io_error",
        }
    }
}

pub(crate) struct PreparedSkillChange {
    plan: SkillSwitchPlan,
    receipt: SkillLiveReceipt<PathBuf>,
    paths: LivePaths,
}

impl PreparedSkillChange {
    pub(crate) fn plan(&self) -> &SkillSwitchPlan {
        &self.plan
    }

    pub(crate) fn decide_catalog(&self, entry: Option<&SkillCatalogEntry>) -> SkillCatalogDecision {
        self.plan.decide_catalog(entry)
    }

    pub(crate) fn commit(self) -> Result<(), SkillHostError> {
        self.receipt
            .commit()
            .map_err(|error| SkillHostError::Live(error.to_string()))
    }

    pub(crate) fn rollback(self) -> Result<(), SkillHostError> {
        let mut host = FileOperationHost::new(&self.paths);
        self.receipt
            .rollback(&mut host)
            .map_err(|error| SkillHostError::Live(error.to_string()))
    }
}

pub(crate) struct SkillLiveConfig {
    source_root: PathBuf,
    unified_root: PathBuf,
    app_roots: Vec<(AppType, PathBuf, PathBuf)>,
    paths: LivePaths,
}

impl SkillLiveConfig {
    pub(crate) fn from_home(
        home: &Path,
        dirs: &ResolvedConfigDirs,
        paths: &LivePaths,
        storage: Option<&str>,
    ) -> Result<Self, SkillHostError> {
        let unified_root = home.join(".agents").join("skills");
        let source_root = match storage {
            None | Some("cc_switch") => home.join(".cc-switch").join("skills"),
            Some("unified") => unified_root.clone(),
            Some(value) => return Err(SkillHostError::InvalidStorage(value.to_owned())),
        };
        let app_roots = builtin_app_registry()
            .descriptors()
            .filter(|descriptor| descriptor.skill_contract().is_some())
            .map(|descriptor| {
                let app = descriptor.app().clone();
                let native_root = absolute_root(native_root(dirs, &app))?.join("skills");
                let state_root = native_root
                    .parent()
                    .expect("a native Skill root has a parent")
                    .join(STATE_DIRECTORY)
                    .join(app.as_str());
                Ok((app, native_root, state_root))
            })
            .collect::<Result<Vec<_>, SkillHostError>>()?;
        Ok(Self {
            source_root,
            unified_root,
            app_roots,
            paths: paths.clone(),
        })
    }

    pub(crate) fn inspect(
        &self,
        catalog: &[SkillCatalogEntry],
    ) -> Result<Vec<InstalledSkillSnapshot>, SkillHostError> {
        let runtime = self.runtime()?;
        inspect_installed_skills(catalog, &runtime)
            .map_err(|error| SkillHostError::Observation(error.to_string()))
    }

    pub(crate) fn apply(
        &self,
        catalog: &[SkillCatalogEntry],
        skill_id: &str,
        app: &AppType,
        requested: Option<bool>,
    ) -> Result<PreparedSkillChange, SkillHostError> {
        self.prepare_roots(app)?;
        let runtime = self.runtime()?;
        let plan = match requested {
            Some(enabled) => prepare_skill_switch(catalog, skill_id, &runtime, app, enabled),
            None => prepare_skill_reconciliation(catalog, skill_id, &runtime, app),
        }
        .map_err(|error| SkillHostError::Prepare(error.to_string()))?;
        let mut host = FileOperationHost::new(&self.paths);
        let receipt = execute_skill_live_plan(&plan, &mut host)
            .map_err(|error| SkillHostError::Live(error.to_string()))?;
        Ok(PreparedSkillChange {
            plan,
            receipt,
            paths: self.paths.clone(),
        })
    }

    fn runtime(&self) -> Result<SkillRuntime, SkillHostError> {
        let apps = self
            .app_roots
            .iter()
            .map(|(app, native_root, state_root)| {
                let descriptor = builtin_app_registry().for_app(app);
                let config = descriptor
                    .skill_contract()
                    .and_then(|contract| contract.config_target())
                    .map(|target| {
                        let target = target.logical_target();
                        read_optional(self.paths.path_for(target)).map(|contents| {
                            contents.map_or_else(
                                || ObservedDocument::missing(target),
                                |contents| ObservedDocument::present(target, contents),
                            )
                        })
                    })
                    .transpose()?;
                SkillAppRuntime::try_new(
                    app.clone(),
                    native_root.clone(),
                    state_root.clone(),
                    config,
                )
                .map_err(|error| SkillHostError::Runtime(error.to_string()))
            })
            .collect::<Result<Vec<_>, SkillHostError>>()?;
        SkillRuntime::try_new(self.source_root.clone(), self.unified_root.clone(), apps)
            .map_err(|error| SkillHostError::Runtime(error.to_string()))
    }

    fn prepare_roots(&self, app: &AppType) -> Result<(), SkillHostError> {
        let (_, native_root, state_root) = self
            .app_roots
            .iter()
            .find(|(candidate, _, _)| candidate == app)
            .ok_or_else(|| SkillHostError::UnsupportedApp(app.as_str().to_owned()))?;
        create_real_directory(native_root, false)?;
        create_real_directory(state_root, true)
    }
}

fn absolute_root(path: &Path) -> Result<PathBuf, SkillHostError> {
    std::path::absolute(path).map_err(|source| SkillHostError::Io {
        path: path.to_owned(),
        source,
    })
}

fn native_root<'a>(dirs: &'a ResolvedConfigDirs, app: &AppType) -> &'a Path {
    match app {
        AppType::Claude => &dirs.claude,
        AppType::Codex => &dirs.codex,
        AppType::Gemini => &dirs.gemini,
        AppType::GrokBuild => &dirs.grok,
        AppType::OpenCode => &dirs.opencode,
        AppType::Hermes => &dirs.hermes,
        AppType::Pi => &dirs.pi,
        AppType::ClaudeDesktop | AppType::OpenClaw => {
            unreachable!("only applications with Skill contracts are collected")
        }
    }
}

fn create_real_directory(path: &Path, private: bool) -> Result<(), SkillHostError> {
    fs::create_dir_all(path).map_err(|source| SkillHostError::Io {
        path: path.to_owned(),
        source,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| SkillHostError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(SkillHostError::Runtime(format!(
            "{} must be a real directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            SkillHostError::Io {
                path: path.to_owned(),
                source,
            }
        })?;
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(())
}
