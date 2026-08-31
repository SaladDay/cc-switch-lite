use std::{collections::BTreeMap, fs, path::PathBuf};

use cc_switch_core::{
    apply_skill_deployment, builtin_app_registry, inspect_skill_deployment,
    validate_skill_directory, validate_skill_source, AppType, SkillActivationSource,
    SkillConfigError, SkillDeploymentReceipt, SkillDeploymentState, SkillSyncMethod,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillLiveError {
    #[error("application '{0}' does not support Skills")]
    Unsupported(String),
    #[error("{0} is not installed or its configuration directory is unavailable")]
    AppUnavailable(String),
    #[error("{0} configuration path is not a directory: {1:?}")]
    InvalidAppRoot(String, PathBuf),
    #[error("Skill application path is unavailable for '{0}'")]
    MissingTarget(String),
    #[error(transparent)]
    Config(#[from] SkillConfigError),
    #[error("Skill filesystem I/O failed at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(crate) struct SkillObservation {
    pub(crate) id: String,
    pub(crate) source_issue: Option<String>,
    pub(crate) native_apps: BTreeMap<String, Result<bool, String>>,
}

struct SkillTarget {
    app: AppType,
    install_root: PathBuf,
    skills_root: PathBuf,
}

pub(crate) struct SkillLiveConfig {
    source_root: PathBuf,
    sync_method: SkillSyncMethod,
    targets: Vec<SkillTarget>,
}

impl SkillLiveConfig {
    pub(crate) fn new(
        source_root: PathBuf,
        sync_method: SkillSyncMethod,
        app_roots: Vec<(AppType, PathBuf)>,
    ) -> Self {
        Self {
            source_root,
            sync_method,
            targets: app_roots
                .into_iter()
                .map(|(app, install_root)| SkillTarget {
                    skills_root: install_root.join("skills"),
                    app,
                    install_root,
                })
                .collect(),
        }
    }

    pub(crate) fn observe(&self, skills: &[(String, String)]) -> Vec<SkillObservation> {
        skills
            .iter()
            .map(|(id, directory)| {
                let source_issue = validate_skill_directory(directory)
                    .and_then(|()| validate_skill_source(&self.source_root.join(directory)))
                    .err()
                    .map(|error| error.to_string());
                let native_apps = if source_issue.is_some() {
                    BTreeMap::new()
                } else {
                    self.observe_native_apps(directory)
                };
                SkillObservation {
                    id: id.clone(),
                    source_issue,
                    native_apps,
                }
            })
            .collect()
    }

    pub(crate) fn apply(
        &self,
        directory: &str,
        app: &AppType,
        enabled: bool,
    ) -> Result<SkillDeploymentReceipt, SkillLiveError> {
        let descriptor = builtin_app_registry().for_app(app);
        if descriptor.skill_contract().is_none() {
            return Err(SkillLiveError::Unsupported(app.as_str().to_owned()));
        }
        let target = self
            .target(app)
            .ok_or_else(|| SkillLiveError::MissingTarget(app.as_str().to_owned()))?;
        if enabled {
            match fs::metadata(&target.install_root) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    return Err(SkillLiveError::InvalidAppRoot(
                        descriptor.display_name().to_owned(),
                        target.install_root.clone(),
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SkillLiveError::AppUnavailable(
                        descriptor.display_name().to_owned(),
                    ))
                }
                Err(source) => {
                    return Err(SkillLiveError::Io {
                        path: target.install_root.clone(),
                        source,
                    })
                }
            }
        }
        apply_skill_deployment(
            &self.source_root,
            &target.skills_root,
            directory,
            enabled,
            self.sync_method,
        )
        .map_err(Into::into)
    }

    fn observe_native_apps(&self, directory: &str) -> BTreeMap<String, Result<bool, String>> {
        builtin_app_registry()
            .descriptors()
            .filter_map(|descriptor| {
                let contract = descriptor.skill_contract()?;
                (contract.activation_source() == SkillActivationSource::NativePresence)
                    .then_some(descriptor)
            })
            .map(|descriptor| {
                let result = self
                    .target(descriptor.app())
                    .ok_or_else(|| SkillLiveError::MissingTarget(descriptor.id().to_owned()))
                    .and_then(|target| {
                        inspect_skill_deployment(&self.source_root, &target.skills_root, directory)
                            .map(|state| state != SkillDeploymentState::Missing)
                            .map_err(Into::into)
                    })
                    .map_err(|error| error.to_string());
                (descriptor.id().to_owned(), result)
            })
            .collect()
    }

    fn target(&self, app: &AppType) -> Option<&SkillTarget> {
        self.targets.iter().find(|target| &target.app == app)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn enabling_requires_an_existing_application_root() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        let live = SkillLiveConfig::new(
            source,
            SkillSyncMethod::Copy,
            vec![(AppType::Claude, directory.path().join("missing-claude"))],
        );

        assert!(matches!(
            live.apply("docs", &AppType::Claude, true),
            Err(SkillLiveError::AppUnavailable(_))
        ));
        assert!(!directory.path().join("missing-claude").exists());
    }

    #[test]
    fn native_presence_is_observed_from_the_core_contract() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let pi = directory.path().join("pi");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(&pi).unwrap();
        let live = SkillLiveConfig::new(source, SkillSyncMethod::Copy, vec![(AppType::Pi, pi)]);
        live.apply("docs", &AppType::Pi, true)
            .unwrap()
            .commit()
            .unwrap();

        let observations = live.observe(&[("skill".to_owned(), "docs".to_owned())]);
        assert_eq!(observations[0].native_apps["pi"], Ok(true));
    }
}
