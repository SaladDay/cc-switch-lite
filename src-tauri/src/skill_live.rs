use std::{collections::BTreeMap, fs, path::PathBuf};

use cc_switch_core::{
    apply_skill_deployment, builtin_app_registry, inspect_skill_deployment, inspect_skill_presence,
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
    #[error("Skill application targets are invalid: {0}")]
    InvalidTargets(String),
    #[error(
        "{0} discovers the unified Skill store directly; its per-application switch is read-only"
    )]
    UnifiedDiscovery(String),
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
    pub(crate) native_apps: BTreeMap<String, SkillAppObservation>,
}

pub(crate) struct SkillAppObservation {
    pub(crate) enabled: bool,
    pub(crate) issue: Option<String>,
}

struct SkillTarget {
    app: AppType,
    install_root: PathBuf,
    skills_root: PathBuf,
}

pub(crate) struct SkillLiveConfig {
    source_root: PathBuf,
    sync_method: SkillSyncMethod,
    unified_store: bool,
    targets: Vec<SkillTarget>,
}

impl SkillLiveConfig {
    pub(crate) fn new(
        source_root: PathBuf,
        sync_method: SkillSyncMethod,
        unified_store: bool,
        app_roots: Vec<(AppType, PathBuf)>,
    ) -> Result<Self, SkillLiveError> {
        validate_targets(&app_roots)?;
        Ok(Self {
            source_root,
            sync_method,
            unified_store,
            targets: app_roots
                .into_iter()
                .map(|(app, install_root)| SkillTarget {
                    skills_root: install_root.join("skills"),
                    app,
                    install_root,
                })
                .collect(),
        })
    }

    pub(crate) fn observe(&self, skills: &[(String, String)]) -> Vec<SkillObservation> {
        skills
            .iter()
            .map(|(id, directory)| {
                let directory_issue = validate_skill_directory(directory)
                    .err()
                    .map(|error| error.to_string());
                let source_issue = directory_issue.clone().or_else(|| {
                    validate_skill_source(&self.source_root.join(directory))
                        .err()
                        .map(|error| error.to_string())
                });
                let native_apps = if directory_issue.is_some() {
                    BTreeMap::new()
                } else {
                    self.observe_native_apps(directory, source_issue.is_none())
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
        let Some(contract) = descriptor.skill_contract() else {
            return Err(SkillLiveError::Unsupported(app.as_str().to_owned()));
        };
        if self.unified_store && contract.discovers_unified_store() {
            return Err(SkillLiveError::UnifiedDiscovery(
                descriptor.display_name().to_owned(),
            ));
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

    fn observe_native_apps(
        &self,
        directory: &str,
        source_valid: bool,
    ) -> BTreeMap<String, SkillAppObservation> {
        builtin_app_registry()
            .descriptors()
            .filter_map(|descriptor| {
                let contract = descriptor.skill_contract()?;
                (contract.activation_source() == SkillActivationSource::NativePresence
                    || (self.unified_store && contract.discovers_unified_store()))
                .then_some((descriptor, contract))
            })
            .map(|(descriptor, contract)| {
                let observation = if self.unified_store && contract.discovers_unified_store() {
                    match inspect_skill_presence(&self.source_root, directory) {
                        Ok(enabled) => SkillAppObservation {
                            enabled,
                            issue: enabled.then(|| {
                                SkillLiveError::UnifiedDiscovery(
                                    descriptor.display_name().to_owned(),
                                )
                                .to_string()
                            }),
                        },
                        Err(error) => SkillAppObservation {
                            enabled: false,
                            issue: Some(error.to_string()),
                        },
                    }
                } else {
                    self.target(descriptor.app())
                        .map(|target| {
                            observe_managed_presence(
                                &self.source_root,
                                &target.skills_root,
                                directory,
                                source_valid,
                            )
                        })
                        .unwrap_or_else(|| SkillAppObservation {
                            enabled: false,
                            issue: Some(
                                SkillLiveError::MissingTarget(descriptor.id().to_owned())
                                    .to_string(),
                            ),
                        })
                };
                (descriptor.id().to_owned(), observation)
            })
            .collect()
    }

    fn target(&self, app: &AppType) -> Option<&SkillTarget> {
        self.targets.iter().find(|target| &target.app == app)
    }
}

fn observe_managed_presence(
    source_root: &std::path::Path,
    destination_root: &std::path::Path,
    directory: &str,
    source_valid: bool,
) -> SkillAppObservation {
    match inspect_skill_presence(destination_root, directory) {
        Ok(false) => SkillAppObservation {
            enabled: false,
            issue: None,
        },
        Ok(true) => {
            let issue = if source_valid {
                match inspect_skill_deployment(source_root, destination_root, directory) {
                    Ok(SkillDeploymentState::Missing) => {
                        Some("native Skill changed while it was inspected".to_owned())
                    }
                    Ok(SkillDeploymentState::Linked | SkillDeploymentState::Copied) => None,
                    Err(error) => Some(error.to_string()),
                }
            } else {
                None
            };
            SkillAppObservation {
                enabled: true,
                issue,
            }
        }
        Err(error) => SkillAppObservation {
            enabled: false,
            issue: Some(error.to_string()),
        },
    }
}

fn validate_targets(app_roots: &[(AppType, PathBuf)]) -> Result<(), SkillLiveError> {
    for descriptor in builtin_app_registry().descriptors() {
        let expected = usize::from(descriptor.skill_contract().is_some());
        let actual = app_roots
            .iter()
            .filter(|(app, _)| app == descriptor.app())
            .count();
        if actual != expected {
            return Err(SkillLiveError::InvalidTargets(format!(
                "application '{}' requires {expected} target(s), found {actual}",
                descriptor.id()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn app_roots(base: &std::path::Path) -> Vec<(AppType, PathBuf)> {
        builtin_app_registry()
            .descriptors()
            .filter(|descriptor| descriptor.skill_contract().is_some())
            .map(|descriptor| (descriptor.app().clone(), base.join(descriptor.id())))
            .collect()
    }

    #[test]
    fn enabling_requires_an_existing_application_root() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        let live = SkillLiveConfig::new(
            source,
            SkillSyncMethod::Copy,
            false,
            app_roots(directory.path()),
        )
        .unwrap();

        assert!(matches!(
            live.apply("docs", &AppType::Claude, true),
            Err(SkillLiveError::AppUnavailable(_))
        ));
        assert!(!directory.path().join("claude").exists());
    }

    #[test]
    fn native_presence_is_observed_from_the_core_contract() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let pi = directory.path().join("pi");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(&pi).unwrap();
        let mut roots = app_roots(directory.path());
        roots
            .iter_mut()
            .find(|(app, _)| app == &AppType::Pi)
            .unwrap()
            .1 = pi.clone();
        let live = SkillLiveConfig::new(source, SkillSyncMethod::Copy, false, roots).unwrap();
        live.apply("docs", &AppType::Pi, true)
            .unwrap()
            .commit()
            .unwrap();

        let observations = live.observe(&[("skill".to_owned(), "docs".to_owned())]);
        assert!(observations[0].native_apps["pi"].enabled);
        assert_eq!(observations[0].native_apps["pi"].issue, None);

        fs::write(pi.join("skills/docs/extra"), "external change").unwrap();
        let observations = live.observe(&[("skill".to_owned(), "docs".to_owned())]);
        assert!(observations[0].native_apps["pi"].enabled);
        assert!(observations[0].native_apps["pi"].issue.is_some());
    }

    #[test]
    fn unified_store_is_reported_as_native_read_only_pi_state() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        let live = SkillLiveConfig::new(
            source,
            SkillSyncMethod::Copy,
            true,
            app_roots(directory.path()),
        )
        .unwrap();

        let observations = live.observe(&[("skill".to_owned(), "docs".to_owned())]);
        let pi = &observations[0].native_apps["pi"];
        assert!(pi.enabled);
        assert!(pi.issue.as_deref().unwrap().contains("read-only"));
        assert!(matches!(
            live.apply("docs", &AppType::Pi, false),
            Err(SkillLiveError::UnifiedDiscovery(_))
        ));
    }

    #[test]
    fn every_core_skill_application_requires_exactly_one_target() {
        assert!(matches!(
            SkillLiveConfig::new(
                PathBuf::from("/source"),
                SkillSyncMethod::Copy,
                false,
                Vec::new()
            ),
            Err(SkillLiveError::InvalidTargets(_))
        ));
    }
}
